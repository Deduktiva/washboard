//! Send, the response pane, the request's history and the app's HTTP log (PLAN §4 "Send",
//! "HTTP log").

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use washboard_core::diag::LineIndex;
use washboard_core::http::{self, Exchange, SendRequest, SoapFault, detect_fault};
use washboard_core::model::{Auth, HistoryEntry, HistoryId, RequestId, Server, ServerId};
use washboard_core::project::HistoryRecord;
use washboard_core::validate::validate_request;
use washboard_core::xml;

use crate::app::{App, ModelError, ProjectKey};
use crate::diagnostics::{Check, Issue};
use crate::event::Event;
use crate::window::{ProjectWindow, SchemaState};

/// The HTTP log keeps this many exchanges, across all projects.
pub const LOG_CAPACITY: usize = 50;

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
    pub headers: Vec<(String, String)>,
    /// Pretty-printed if it is well-formed XML, else as received.
    pub body: Option<String>,
    /// Body size in bytes, as received.
    pub size: usize,
    pub fault: Option<SoapFault>,
}

impl ResponseView {
    fn from_record(record: HistoryRecord) -> ResponseView {
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
            headers: record.response_headers,
            size: record.response_body.as_ref().map_or(0, Vec::len),
            body: record.response_body.as_deref().map(display_body),
            fault,
        }
    }

    fn from_exchange(
        history: Option<HistoryId>,
        server: &Server,
        exchange: &Exchange,
        fault: Option<SoapFault>,
    ) -> ResponseView {
        let response = exchange.response.as_ref();
        ResponseView {
            history,
            server: Some(server.id),
            url: server.url.clone(),
            sent_at: exchange.started_at,
            duration: Some(exchange.duration),
            status: response.and_then(|r| status_code(&r.start_line)),
            error: exchange.error.clone(),
            headers: response.map(|r| r.headers.clone()).unwrap_or_default(),
            size: response.map_or(0, |r| r.body.len()),
            body: response.map(|r| display_body(&r.body)),
            fault,
        }
    }
}

/// `HTTP/1.1 200 OK` → 200.
fn status_code(start_line: &str) -> Option<u16> {
    start_line.split_whitespace().nth(1)?.parse().ok()
}

fn display_body(bytes: &[u8]) -> String {
    match xml::decode(bytes) {
        Ok(decoded) => xml::pretty_print(&decoded.text).unwrap_or(decoded.text),
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
        self.exchange
            .request
            .headers
            .iter()
            .map(|(name, value)| {
                if reveal || !name.eq_ignore_ascii_case("authorization") {
                    return (name.clone(), value.clone());
                }
                let scheme = value.split_whitespace().next().unwrap_or_default();
                (
                    name.clone(),
                    format!("{scheme} ••••••••").trim_start().to_owned(),
                )
            })
            .collect()
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
    /// selected server. Errors show the issues list instead ([`Event::ShowIssues`]).
    pub fn send(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        self.flush(key)?;
        let id = self.next();
        let secrets = self.front.secrets.clone();
        let window = self.window_mut(key).ok_or(ModelError::UnknownProject)?;
        if window.sending.is_some() {
            return Err(ModelError::AlreadySending);
        }
        let editor = window
            .editor
            .as_ref()
            .ok_or(ModelError::NoRequestSelected)?;
        let schema = match &window.schema {
            SchemaState::Ready(schema) if schema.compile_errors().is_empty() => schema.clone(),
            SchemaState::Ready(_) => {
                return Err(ModelError::SchemaFailed(
                    "the WSDL's schemas could not be compiled".into(),
                ));
            }
            SchemaState::Loading => return Err(ModelError::SchemaNotReady),
            SchemaState::Failed(m) => return Err(ModelError::SchemaFailed(m.clone())),
        };
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
                    let lines = LineIndex::new(&text);
                    let issues = validation
                        .diagnostics
                        .into_iter()
                        .map(|d| Issue::new(&text, &lines, d))
                        .collect();
                    return Outcome::Refused(issues);
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
                history, &server, &exchange, fault,
            ));
            window.reload_history();
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

    /// Shows a history entry in the response pane.
    pub fn show_history(&mut self, key: ProjectKey, entry: HistoryId) -> Result<(), ModelError> {
        let window = self.window_mut(key).ok_or(ModelError::UnknownProject)?;
        let record = window.project.load_history(entry)?;
        window.response = Some(ResponseView::from_record(record));
        self.events.push(Event::ResponseChanged { project: key });
        Ok(())
    }

    /// History ▸ Restore request: puts the request as it was sent into the editor (an edit,
    /// so autosave and checks follow; the widget's undo does not cover it).
    pub fn restore_request(&mut self, key: ProjectKey, entry: HistoryId) -> Result<(), ModelError> {
        let window = self.window_mut(key).ok_or(ModelError::UnknownProject)?;
        let record = window.project.load_history(entry)?;
        let text = xml::decode(&record.request_body)
            .map(|d| d.text)
            .unwrap_or_else(|_| String::from_utf8_lossy(&record.request_body).into_owned());
        let editor = window
            .editor
            .as_ref()
            .ok_or(ModelError::NoRequestSelected)?;
        if editor.request() != record.entry.request_id {
            return Err(ModelError::NoRequestSelected);
        }
        let len = editor.utf16_len();
        self.edit(key, 0..len, &text)?;
        self.events.push(Event::EditorReplaced { project: key });
        Ok(())
    }

    /// Oldest first.
    pub fn http_log(&self) -> &VecDeque<LogEntry> {
        &self.log
    }

    /// Loads the selected request's history and shows its latest entry.
    pub(crate) fn load_history(window: &mut ProjectWindow) {
        window.reload_history();
        window.response = window
            .history
            .first()
            .and_then(|e| window.project.load_history(e.id).ok())
            .map(ResponseView::from_record);
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
