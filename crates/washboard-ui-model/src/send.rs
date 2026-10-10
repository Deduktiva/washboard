//! Send, the response pane, the request's history and the app's HTTP log (PLAN §4 "Send",
//! "HTTP log").

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use washboard_core::http::{self, Exchange, RawMessage, SendRequest, SoapFault, detect_fault};
use washboard_core::model::{Auth, HistoryEntry, HistoryId, RequestId, Server, ServerId};
use washboard_core::project::{HistoryRecord, RequestHead};
use washboard_core::validate::validate_request;
use washboard_core::xml;

use crate::app::{App, ModelError, ProjectKey};
use crate::diagnostics::{Check, Issue};
use crate::event::Event;
use crate::window::ProjectWindow;

/// The HTTP log keeps this many exchanges, across all projects.
pub const LOG_CAPACITY: usize = 50;

/// An older exchange of the selected request, shown whole in place of the latest one (PLAN §4
/// "Response pane and history"): its response in the response pane, its request as sent
/// beside it, read-only. While one is shown, Send is refused.
#[derive(Debug, Clone)]
pub struct OlderExchange {
    pub entry: HistoryId,
    /// The request as it was sent, decoded for display.
    pub request: String,
    /// What the response pane showed before, shown again by Show Latest. Kept rather than
    /// re-read, since the latest send may not have made it into the history.
    latest: Option<ResponseView>,
}

/// The History drawer under the response, kept per project in `ui_state`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HistoryDrawer {
    pub open: bool,
    /// The table's height while open, in points; `None` until the user has resized it.
    pub height: Option<f64>,
}

const DRAWER_OPEN_KEY: &str = "history_drawer.open";
const DRAWER_HEIGHT_KEY: &str = "history_drawer.height";

/// What the response pane shows: the last send of the selected request, or a history entry.
#[derive(Debug, Clone)]
pub struct ResponseView {
    /// `None` if the exchange could not be stored in the history.
    pub history: Option<HistoryId>,
    /// The server it went to; `None` if that server has been deleted since. Name it with
    /// [`ProjectWindow::server_label`](crate::ProjectWindow::server_label).
    pub server: Option<ServerId>,
    pub url: String,
    pub sent_at: SystemTime,
    pub duration: Option<Duration>,
    pub status: Option<u16>,
    /// Transport error (connect, TLS, timeout); then there is no response.
    pub error: Option<String>,
    /// The request's start line and headers as sent; `None` for history entries recorded
    /// before the history kept them.
    pub request: Option<RequestHead>,
    pub headers: Vec<(String, String)>,
    /// Pretty-printed if it is well-formed XML, else as received.
    pub body: Option<String>,
    /// Body size in bytes, as received.
    pub size: usize,
    pub fault: Option<SoapFault>,
}

impl ResponseView {
    fn from_record(record: HistoryRecord, indent: usize) -> ResponseView {
        let entry = record.entry;
        let fault = record.response_body.as_deref().and_then(detect_fault);
        ResponseView {
            history: Some(entry.id),
            server: entry.server_id,
            url: entry.url,
            sent_at: entry.sent_at,
            duration: entry.duration,
            status: entry.http_status,
            error: entry.error,
            request: record.request_head,
            headers: record.response_headers,
            size: record.response_body.as_ref().map_or(0, Vec::len),
            body: record
                .response_body
                .as_deref()
                .map(|b| display_body(b, indent)),
            fault,
        }
    }

    fn from_exchange(
        history: Option<HistoryId>,
        server: &Server,
        exchange: &Exchange,
        fault: Option<SoapFault>,
        indent: usize,
    ) -> ResponseView {
        let response = exchange.response.as_ref();
        ResponseView {
            history,
            server: Some(server.id),
            url: server.url.clone(),
            sent_at: exchange.started_at,
            duration: Some(exchange.duration),
            status: response.and_then(RawMessage::status_code),
            error: exchange.error.clone(),
            request: Some(RequestHead::of(&exchange.request)),
            headers: response.map(|r| r.headers.clone()).unwrap_or_default(),
            size: response.map_or(0, |r| r.body.len()),
            body: response.map(|r| display_body(&r.body, indent)),
            fault,
        }
    }

    /// The Headers tab: the request's start line and headers as sent, then the response's.
    pub fn headers_text(&self) -> String {
        let mut text = String::from("Request\n");
        match &self.request {
            Some(head) => {
                text.push_str(&head.start_line);
                text.push('\n');
                push_headers(&mut text, &head.headers);
            }
            None => text.push_str("not recorded\n"),
        }
        text.push_str("\nResponse\n");
        if self.error.is_some() {
            text.push_str("no response\n");
        }
        push_headers(&mut text, &self.headers);
        text
    }
}

fn push_headers(text: &mut String, headers: &[(String, String)]) {
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\n"));
    }
}

/// Pretty-printed at the app's indent width, so responses look like formatted requests;
/// a lossy fallback is shown as received.
fn display_body(bytes: &[u8], indent: usize) -> String {
    match xml::decode(bytes) {
        Ok(decoded) => xml::pretty_print(&decoded.text, indent).unwrap_or(decoded.text),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// One exchange in the HTTP log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// The project's name, since the log is shared by all windows.
    pub project: String,
    pub exchange: Exchange,
}

impl LogEntry {
    /// The request headers with the `Authorization` value masked unless `reveal`.
    pub fn request_headers(&self, reveal: bool) -> Vec<(String, String)> {
        let request = &self.exchange.request;
        if reveal {
            request.headers.clone()
        } else {
            request.masked_headers()
        }
    }
}

/// What a send worker came back with.
enum Outcome {
    /// Validation found errors; nothing was sent.
    Refused(Vec<Issue>),
    Sent {
        exchange: Box<Exchange>,
        fault: Option<SoapFault>,
    },
}

/// The send in flight for a project window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sending {
    id: u64,
}

impl App {
    /// Send (⌘↩): saves, validates on a worker and, if there are no errors, sends to the
    /// selected server. Errors show the issues list instead ([`Event::ShowIssues`]). Refused
    /// while an older exchange is shown.
    pub fn send(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        // Checked before anything else, so a shortcut can never send an old exchange.
        if self.window(key)?.older.is_some() {
            return Err(ModelError::OlderExchangeShown);
        }
        self.flush(key)?;
        let id = self.next();
        let secrets = self.front.secrets.clone();
        let window = self.window(key)?;
        if window.sending.is_some() {
            return Err(ModelError::AlreadySending);
        }
        let editor = window
            .editor
            .as_ref()
            .ok_or(ModelError::NoRequestSelected)?;
        let schema = window.schema.validating()?.clone();
        let server_id = window.selected_server().ok_or(ModelError::NoServer)?;
        let server = window
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .cloned()
            .ok_or(ModelError::NoServer)?;
        let password = match server.auth {
            Auth::Basic { .. } => window
                .project
                .server_password(server.id, secrets.as_ref())?,
            Auth::None => None,
        };
        let request = editor.request();
        let version = editor.version();
        let text = editor.text().to_owned();
        let hint = window.project.request(request)?.operation;
        window.sending = Some(Sending { id });
        self.events.push(Event::SendStateChanged { project: key });

        let to = server.clone();
        self.spawn(
            move || {
                let validation = {
                    let Some(request_schema) = schema.request_schema() else {
                        return Outcome::Refused(Vec::new());
                    };
                    validate_request(&schema.wsdl, &request_schema, &text, hint.as_ref())
                };
                if validation.has_errors() {
                    return Outcome::Refused(Issue::all(&text, validation.diagnostics));
                }
                let exchange = http::send(&SendRequest {
                    server: to,
                    password,
                    soap_action: validation.soap_action().map(str::to_owned),
                    body: text,
                });
                let fault = exchange
                    .response
                    .as_ref()
                    .and_then(|r| detect_fault(&r.body));
                Outcome::Sent {
                    exchange: Box::new(exchange),
                    fault,
                }
            },
            move |app, outcome| app.send_done(key, id, request, version, server, outcome),
        );
        Ok(())
    }

    /// Stop waiting for the send in flight. The request may still reach the server; its
    /// response is ignored and not recorded.
    pub fn cancel_send(&mut self, key: ProjectKey) {
        if let Some(window) = self.window_mut(key)
            && window.sending.take().is_some()
        {
            self.events.push(Event::SendStateChanged { project: key });
        }
    }

    fn send_done(
        &mut self,
        key: ProjectKey,
        id: u64,
        request: RequestId,
        version: u64,
        server: Server,
        outcome: Outcome,
    ) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        if window.sending != Some(Sending { id }) {
            return;
        }
        window.sending = None;
        self.events.push(Event::SendStateChanged { project: key });
        let (exchange, fault) = match outcome {
            Outcome::Refused(issues) => {
                self.check_done(key, request, version, Check::Full, issues);
                self.events.push(Event::ShowIssues { project: key });
                return;
            }
            Outcome::Sent { exchange, fault } => (exchange, fault),
        };

        let Some(window) = self.window_mut(key) else {
            return;
        };
        let recorded = window
            .project
            .record_exchange(request, &server, &exchange, fault.is_some());
        let name = window.name.clone();
        let indent = self.format.indent;
        let history = match recorded {
            Ok(entry) => Some(entry.id),
            Err(e) => {
                self.alert_error("The response could not be stored in the history", &e.into());
                None
            }
        };
        if let Some(window) = self.window_mut(key)
            && window.selected_request() == Some(request)
        {
            window.response = Some(ResponseView::from_exchange(
                history, &server, &exchange, fault, indent,
            ));
            window.reload_history();
            // A send that finishes while an older exchange is shown returns to the latest.
            if window.older.take().is_some() {
                self.events
                    .push(Event::ShownExchangeChanged { project: key });
            }
            self.events.push(Event::ResponseChanged { project: key });
            self.events.push(Event::HistoryChanged { project: key });
        }
        // Recording made it the request's and the project's last server.
        self.events
            .push(Event::ServerSelectionChanged { project: key });

        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            project: name,
            exchange: *exchange,
        });
        self.events.push(Event::LogAppended);
    }

    /// Shows a history entry of the selected request: the newest returns to the latest
    /// exchange, any other is shown whole as an [`OlderExchange`].
    pub fn show_history(&mut self, key: ProjectKey, entry: HistoryId) -> Result<(), ModelError> {
        let indent = self.format.indent;
        let window = self.window(key)?;
        if window.history.first().map(|e| e.id) == Some(entry) {
            return self.show_latest(key);
        }
        let record = window.project.load_history(entry)?;
        if Some(record.entry.request_id) != window.selected_request() {
            return Err(ModelError::NoRequestSelected);
        }
        let request = xml::decode_lossy(&record.request_body);
        let latest = match window.older.take() {
            Some(older) => older.latest,
            None => window.response.take(),
        };
        window.response = Some(ResponseView::from_record(record, indent));
        window.older = Some(OlderExchange {
            entry,
            request,
            latest,
        });
        self.events.push(Event::ResponseChanged { project: key });
        self.events
            .push(Event::ShownExchangeChanged { project: key });
        Ok(())
    }

    /// Show Latest (Esc): back to the latest exchange and the editor. Nothing happens if it is
    /// already shown.
    pub fn show_latest(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let Some(older) = window.older.take() else {
            return Ok(());
        };
        window.response = older.latest;
        self.events.push(Event::ResponseChanged { project: key });
        self.events
            .push(Event::ShownExchangeChanged { project: key });
        Ok(())
    }

    /// Restore Request: returns to the latest exchange and hands back the older exchange's
    /// request as sent. The front end puts it into the editor through the widget, as one undo
    /// step, so the editor's previous text is one Undo away; the edit reaches the model as any
    /// other.
    pub fn restore_request(&mut self, key: ProjectKey) -> Result<String, ModelError> {
        let window = self.window(key)?;
        if window.editor.is_none() {
            return Err(ModelError::NoRequestSelected);
        }
        let text = window
            .older
            .as_ref()
            .ok_or(ModelError::NoOlderExchange)?
            .request
            .clone();
        self.show_latest(key)?;
        Ok(text)
    }

    /// Opens, closes or resizes the History drawer, remembered for the project.
    pub fn set_history_drawer(
        &mut self,
        key: ProjectKey,
        drawer: HistoryDrawer,
    ) -> Result<(), ModelError> {
        let project = &mut self.window(key)?.project;
        project.set_ui_state(DRAWER_OPEN_KEY, Some(if drawer.open { "1" } else { "0" }))?;
        let height = drawer
            .height
            .filter(|h| h.is_finite() && *h > 0.0)
            .map(|h| format!("{h:.0}"));
        project.set_ui_state(DRAWER_HEIGHT_KEY, height.as_deref())?;
        Ok(())
    }

    /// Oldest first.
    pub fn http_log(&self) -> &VecDeque<LogEntry> {
        &self.log
    }

    /// Loads the selected request's history and shows its latest entry.
    pub(crate) fn load_history(window: &mut ProjectWindow, indent: usize) {
        window.older = None;
        window.reload_history();
        window.response = window
            .history
            .first()
            .and_then(|e| window.project.load_history(e.id).ok())
            .map(|record| ResponseView::from_record(record, indent));
    }
}

impl ProjectWindow {
    /// The selected request's history, newest first.
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    pub fn response(&self) -> Option<&ResponseView> {
        self.response.as_ref()
    }

    /// The older exchange shown in place of the latest; `None` while the latest is shown.
    pub fn older_exchange(&self) -> Option<&OlderExchange> {
        self.older.as_ref()
    }

    /// Whether the History drawer is open and how tall, as last set. A database that can't
    /// be read gives the default, a closed drawer.
    pub fn history_drawer(&self) -> HistoryDrawer {
        let read = |key| self.project.ui_state(key).ok().flatten();
        HistoryDrawer {
            open: read(DRAWER_OPEN_KEY).as_deref() == Some("1"),
            height: read(DRAWER_HEIGHT_KEY)
                .and_then(|h| h.parse::<f64>().ok())
                .filter(|h| h.is_finite() && *h > 0.0),
        }
    }

    /// A send is in flight (the Send button becomes Cancel).
    pub fn sending(&self) -> bool {
        self.sending.is_some()
    }

    fn reload_history(&mut self) {
        self.history = match self.selected_request() {
            Some(id) => self.project.history(id).unwrap_or_default(),
            None => Vec::new(),
        };
    }
}
