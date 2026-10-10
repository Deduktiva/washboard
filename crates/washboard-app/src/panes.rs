//! The request bar above the editor, the issues bar under it and the response pane below them
//! (PLAN §8).

use std::cell::{OnceCell, RefCell};
use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSButton, NSColor, NSControlSize, NSFont, NSLayoutConstraintOrientation,
    NSLayoutPriorityDefaultLow, NSLineBreakMode, NSScrollView, NSStackView, NSStackViewGravity,
    NSTabView, NSTabViewItem, NSTextField, NSView,
};
use objc2_foundation::{
    NSArray, NSDate, NSDateFormatter, NSDateFormatterStyle, NSObject, NSObjectProtocol, NSString,
    ns_string,
};
use washboard_core::model::HistoryId;
use washboard_ui_model::{
    Issue, IssuesBasis, ProjectKey, RequestSummary, ResponseView, WellFormedness,
};

use crate::app::ModelAccess;
use crate::editor::{EditorController, read_only_text, set_text, text_of};
use crate::layout;
use crate::table::TextTable;
use crate::text::{count, operation_chip, well_formedness_text};

/// The bar above the editor (`docs/gui-draft.html`): the request's name, a chip with its
/// operation, and the live well-formedness state. Plain views with no actions, so no
/// Objective-C class of its own.
#[derive(Debug)]
pub struct RequestBar {
    name: Retained<NSTextField>,
    operation: Retained<NSBox>,
    operation_label: Retained<NSTextField>,
    state: Retained<NSTextField>,
    view: Retained<NSStackView>,
}

impl RequestBar {
    pub fn new(mtm: MainThreadMarker) -> RequestBar {
        let name = NSTextField::labelWithString(ns_string!(""), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        layout::truncating(&name, NSLineBreakMode::ByTruncatingTail);
        let (operation, operation_label) = layout::chip("", mtm);
        operation.setHidden(true);
        let state = NSTextField::labelWithString(ns_string!(""), mtm);
        state.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        let view = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        view.addView_inGravity(&name, NSStackViewGravity::Leading);
        view.addView_inGravity(&operation, NSStackViewGravity::Leading);
        view.addView_inGravity(&state, NSStackViewGravity::Trailing);
        view.setSpacing(10.0);
        view.setEdgeInsets(layout::insets(6.0, 12.0, 6.0, 12.0));
        layout::hug_vertically(&view);
        RequestBar {
            name,
            operation,
            operation_label,
            state,
            view,
        }
    }

    pub fn view(&self) -> &NSStackView {
        &self.view
    }

    /// Shows the open request; `None` with no request open.
    pub fn show(&self, request: Option<&RequestSummary>) {
        let name = request.map_or("", |r| r.name.as_str());
        self.name.setStringValue(&NSString::from_str(name));
        // A request made before its operation was recorded, or from a file, has no hint.
        let chip = request
            .and_then(|r| r.operation.as_ref())
            .map(operation_chip);
        self.operation_label
            .setStringValue(&NSString::from_str(chip.as_deref().unwrap_or_default()));
        self.operation.setHidden(chip.is_none());
        self.set_state(request.map_or(WellFormedness::Pending, |r| r.well_formedness));
    }

    /// The well-formedness state alone, which changes with every check.
    pub fn set_state(&self, state: WellFormedness) {
        self.state
            .setStringValue(&NSString::from_str(&well_formedness_text(state)));
        let colour = match state {
            WellFormedness::Pending => NSColor::secondaryLabelColor(),
            WellFormedness::WellFormed => NSColor::systemGreenColor(),
            WellFormedness::Error { .. } => NSColor::systemRedColor(),
        };
        self.state.setTextColor(Some(&colour));
    }

    pub fn name(&self) -> String {
        self.name.stringValue().to_string()
    }

    /// The operation chip's text; `None` while it is hidden.
    pub fn operation(&self) -> Option<String> {
        (!self.operation.isHidden()).then(|| self.operation_label.stringValue().to_string())
    }

    /// The operation chip, for layout checks.
    pub fn operation_chip(&self) -> &NSBox {
        &self.operation
    }

    pub fn state(&self) -> String {
        self.state.stringValue().to_string()
    }
}

#[derive(Debug)]
pub struct IssuesIvars {
    editor: Retained<EditorController>,
    table: Retained<TextTable>,
    /// The listed issues, in the table's order.
    issues: RefCell<Vec<Issue>>,
    summary: OnceCell<Retained<NSTextField>>,
    /// Hide or Show, for the list.
    hide: OnceCell<Retained<NSButton>>,
    view: OnceCell<Retained<NSStackView>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `IssuesBar` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = IssuesIvars]
    #[derive(Debug)]
    pub struct IssuesBar;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for IssuesBar {}

    impl IssuesBar {
        // SAFETY: action methods take the sender and return nothing.
        #[unsafe(method(toggleIssues:))]
        fn toggle(&self, _sender: Option<&AnyObject>) {
            let list = self.ivars().table.view();
            self.show_list(list.isHidden());
        }
    }
);

impl IssuesBar {
    pub fn new(editor: &EditorController, mtm: MainThreadMarker) -> Retained<Self> {
        let table = TextTable::new(&[], mtm);
        let this = Self::alloc(mtm).set_ivars(IssuesIvars {
            editor: editor.retain(),
            table,
            issues: RefCell::new(Vec::new()),
            summary: OnceCell::new(),
            hide: OnceCell::new(),
            view: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let summary = layout::small_label("", mtm);
        // A long first message keeps the Hide button in view.
        layout::truncating(&summary, NSLineBreakMode::ByTruncatingTail);
        // SAFETY: this bar owns the button through its view, so it outlives the button's weak
        // target reference.
        let hide = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Hide"),
                Some(&this),
                Some(sel!(toggleIssues:)),
                mtm,
            )
        };
        hide.setControlSize(NSControlSize::Small);
        let header = layout::row(
            &[
                Retained::into_super(Retained::into_super(summary.clone())),
                Retained::into_super(Retained::into_super(hide.clone())),
            ],
            mtm,
        );
        header.setEdgeInsets(layout::insets(4.0, 8.0, 4.0, 8.0));

        // The table belongs to this bar: a strong reference back would be a cycle.
        let bar = Weak::from(&*this);
        this.ivars().table.on_click(move |row| {
            if let Some(bar) = bar.load() {
                bar.reveal_issue(row);
            }
        });
        let list: Retained<NSView> = Retained::into_super(this.ivars().table.view().retain());
        layout::set_height(&list, ISSUES_HEIGHT);
        let view = layout::fill_column(&[Retained::into_super(header), list], mtm);
        layout::hug_vertically(&view);
        let _ = this.ivars().summary.set(summary);
        let _ = this.ivars().hide.set(hide);
        let _ = this.ivars().view.set(view);
        this
    }

    pub fn view(&self) -> &NSStackView {
        self.ivars().view.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        &self.ivars().table
    }

    /// The summary beside the list: the validation state of the editor's text.
    pub fn summary(&self) -> String {
        self.ivars()
            .summary
            .get()
            .map(|label| label.stringValue().to_string())
            .unwrap_or_default()
    }

    /// Lists the editor's issues, marks their lines in the ruler and underlines their ranges
    /// (`DiagnosticsChanged`). The summary is the live validation indicator: it says
    /// whether the text has been validated yet, so an empty list never reads as "valid" while a
    /// check is still due. `basis` is `None` with no request open.
    pub fn set_issues(&self, issues: Vec<Issue>, basis: Option<IssuesBasis>) {
        let errors = issues.iter().filter(|i| i.is_error()).count();
        let warnings = issues.len() - errors;
        let (summary, colour) = summary(errors, warnings, basis);
        if let Some(label) = self.ivars().summary.get() {
            label.setStringValue(&NSString::from_str(&summary));
            label.setTextColor(Some(&colour));
        }
        self.ivars().table.set_rows(
            issues
                .iter()
                .map(|i| {
                    let line = i.line.map_or_else(|| "—".to_owned(), |l| l.to_string());
                    vec![line, i.message.clone()]
                })
                .collect(),
        );
        let editor = &self.ivars().editor;
        let errors = issues.iter().filter(|i| i.is_error());
        editor.ruler().set_error_lines(
            errors
                .clone()
                .filter_map(|i| i.line)
                .map(|l| l as usize)
                .collect(),
        );
        editor.set_underlines(errors.filter_map(|i| i.range.clone()).collect());
        *self.ivars().issues.borrow_mut() = issues;
    }

    /// Shows the list if it was hidden (`ShowIssues`).
    pub fn reveal(&self) {
        self.show_list(true);
    }

    /// A hidden list leaves the stack, so the editor takes its height.
    fn show_list(&self, shown: bool) {
        self.ivars().table.view().setHidden(!shown);
        if let Some(hide) = self.ivars().hide.get() {
            hide.setTitle(if shown {
                ns_string!("Hide")
            } else {
                ns_string!("Show")
            });
        }
    }

    /// A click on row `row`: selects the issue's range, else its line.
    pub fn reveal_issue(&self, row: usize) {
        let Some(issue) = self.ivars().issues.borrow().get(row).cloned() else {
            return;
        };
        let editor = &self.ivars().editor;
        match (issue.range, issue.line) {
            (Some(range), _) if !range.is_empty() => editor.select_range(range),
            (_, Some(line)) => editor.select_line(line as usize),
            (Some(range), None) => editor.select_range(range),
            (None, None) => {}
        }
    }
}

/// The issues list's height while shown; the editor gets the rest.
const ISSUES_HEIGHT: f64 = 110.0;

pub const RESPONSE_TABS: [&str; 3] = ["Response", "Headers", "History"];

#[derive(Debug)]
pub struct ResponseIvars {
    key: ProjectKey,
    status: Retained<NSTextField>,
    body: Retained<EditorController>,
    headers: Retained<NSScrollView>,
    history: Retained<TextTable>,
    /// The history rows' entries, in the table's order (newest first).
    history_ids: RefCell<Vec<HistoryId>>,
    tabs: Retained<NSTabView>,
    view: OnceCell<Retained<NSStackView>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ResponsePane` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ResponseIvars]
    #[derive(Debug)]
    pub struct ResponsePane;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ResponsePane {}

    impl ResponsePane {
        // SAFETY: action methods take the sender and return nothing.
        #[unsafe(method(restoreRequest:))]
        fn restore_request(&self, _sender: Option<&AnyObject>) {
            self.restore_selected();
        }
    }
);

impl ResponsePane {
    /// The response pane of `key`'s window.
    pub fn new(key: ProjectKey, mtm: MainThreadMarker) -> Retained<Self> {
        let body = EditorController::new(mtm);
        body.text_view().setEditable(false);
        let headers = read_only_text(mtm);
        let history = TextTable::new(&["Sent", "Server", "Status", "Duration"], mtm);
        let tabs = NSTabView::new(mtm);
        let status = layout::small_label("No response yet", mtm);
        // A long fault message is cut off rather than widening the window.
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentCompressionResistancePriority_forOrientation(
            NSLayoutPriorityDefaultLow,
            NSLayoutConstraintOrientation::Horizontal,
        );

        let this = Self::alloc(mtm).set_ivars(ResponseIvars {
            key,
            status,
            body,
            headers,
            history,
            history_ids: RefCell::new(Vec::new()),
            tabs,
            view: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        // The table belongs to this pane: a strong reference back would be a cycle.
        let pane = Weak::from(&*this);
        this.ivars().history.on_click(move |row| {
            if let Some(pane) = pane.load() {
                pane.show_history_row(row);
            }
        });
        // SAFETY: this pane owns the button through its view, so it outlives the button's
        // weak target reference.
        let restore = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Restore Request"),
                Some(&this),
                Some(sel!(restoreRequest:)),
                mtm,
            )
        };
        let buttons = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        buttons.addView_inGravity(&restore, NSStackViewGravity::Trailing);
        buttons.setEdgeInsets(layout::insets(8.0, 8.0, 8.0, 8.0));
        layout::hug_vertically(&buttons);
        let history_page = layout::fill_column(
            &[
                Retained::into_super(this.ivars().history.view().retain()),
                Retained::into_super(buttons),
            ],
            mtm,
        );

        let pages: [Retained<NSView>; 3] = [
            Retained::into_super(this.ivars().body.view().retain()),
            Retained::into_super(this.ivars().headers.clone()),
            Retained::into_super(history_page),
        ];
        for (label, page) in RESPONSE_TABS.iter().zip(&pages) {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(&layout::tab_page(page, mtm)));
            this.ivars().tabs.addTabViewItem(&item);
        }
        let status = layout::row(
            &[Retained::into_super(Retained::into_super(
                this.ivars().status.clone(),
            ))],
            mtm,
        );
        status.setEdgeInsets(layout::insets(6.0, 8.0, 2.0, 8.0));
        let view = layout::fill_column(
            &[
                Retained::into_super(status),
                Retained::into_super(this.ivars().tabs.clone()),
            ],
            mtm,
        );
        let _ = this.ivars().view.set(view);
        this
    }

    pub fn view(&self) -> &NSStackView {
        self.ivars().view.get().expect("set in new()")
    }

    pub fn status(&self) -> String {
        self.ivars().status.stringValue().to_string()
    }

    pub fn set_status(&self, status: &str) {
        self.ivars()
            .status
            .setStringValue(&NSString::from_str(status));
    }

    pub fn tabs(&self) -> &NSTabView {
        &self.ivars().tabs
    }

    pub fn body(&self) -> &EditorController {
        &self.ivars().body
    }

    /// The Headers tab's text.
    pub fn headers(&self) -> String {
        text_of(&self.ivars().headers)
    }

    pub fn history(&self) -> &TextTable {
        &self.ivars().history
    }

    /// Shows the model's response for the window (`ResponseChanged`, `SendStateChanged`).
    pub fn show_response(&self) {
        let key = self.ivars().key;
        let Some((response, server, sending)) = self
            .read(|app| {
                let window = app.project(key)?;
                let response = window.response().cloned();
                let server = response
                    .as_ref()
                    .map(|r| window.server_label(r.server, &r.url));
                Some((response, server, window.sending()))
            })
            .flatten()
        else {
            return;
        };
        let status = match (&response, server, sending) {
            (_, _, true) => "Sending…".to_owned(),
            (Some(response), Some(server), false) => {
                let sent = date_text(&sent_formatter(), response.sent_at);
                format!("{} · {server} · {sent}", status_line(response))
            }
            _ => "No response yet".to_owned(),
        };
        self.set_status(&status);
        // The full URL, for when the server's name is not enough.
        let url = response.as_ref().map(|r| NSString::from_str(&r.url));
        self.ivars().status.setToolTip(url.as_deref());
        let body = response
            .as_ref()
            .and_then(|r| r.body.clone())
            .unwrap_or_default();
        self.ivars().body.set_text(&body);
        let headers: String = response
            .iter()
            .flat_map(|r| &r.headers)
            .map(|(name, value)| format!("{name}: {value}\n"))
            .collect();
        set_text(&self.ivars().headers, &headers);
    }

    /// Lists the selected request's history (`HistoryChanged`).
    pub fn show_history(&self) {
        let key = self.ivars().key;
        let entries = self
            .read(|app| {
                let window = app.project(key)?;
                let entries = window.history().iter().map(|e| {
                    let server = window.server_label(e.server_id, &e.url);
                    (e.clone(), server)
                });
                Some(entries.collect::<Vec<_>>())
            })
            .flatten()
            .unwrap_or_default();
        let formatter = sent_formatter();
        let rows = entries
            .iter()
            .map(|(e, server)| {
                let status = match (&e.error, e.http_status) {
                    (Some(_), _) => "Failed".to_owned(),
                    (None, Some(code)) if e.soap_fault => format!("{code} Fault"),
                    (None, Some(code)) => code.to_string(),
                    (None, None) => "—".to_owned(),
                };
                let duration = e
                    .duration
                    .map_or_else(String::new, |d| format!("{} ms", d.as_millis()));
                vec![
                    date_text(&formatter, e.sent_at),
                    server.clone(),
                    status,
                    duration,
                ]
            })
            .collect();
        self.ivars().history.set_rows(rows);
        *self.ivars().history_ids.borrow_mut() = entries.iter().map(|(e, _)| e.id).collect();
    }

    /// A click on a history row shows that exchange.
    pub fn show_history_row(&self, row: usize) {
        let Some(entry) = self.ivars().history_ids.borrow().get(row).copied() else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not show the history entry", |app| {
            app.show_history(key, entry)
        });
    }

    /// History ▸ Restore Request, for the selected row.
    pub fn restore_selected(&self) {
        let row = self.ivars().history.table().selectedRow();
        let entry = usize::try_from(row)
            .ok()
            .and_then(|row| self.ivars().history_ids.borrow().get(row).copied());
        let Some(entry) = entry else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not restore the request", |app| {
            app.restore_request(key, entry)
        });
    }
}

/// The issues summary and its colour. Errors show as soon as any check finds them; "Valid"
/// only once the schema check of the current text found none.
fn summary(
    errors: usize,
    warnings: usize,
    basis: Option<IssuesBasis>,
) -> (String, Retained<NSColor>) {
    match (errors, warnings, basis) {
        (_, _, None) => (String::new(), NSColor::secondaryLabelColor()),
        (0, _, Some(IssuesBasis::Pending)) => ("Checking…".into(), NSColor::secondaryLabelColor()),
        (0, _, Some(IssuesBasis::WellFormedOnly)) => (
            "Well-formed · schema not checked".into(),
            NSColor::systemOrangeColor(),
        ),
        (0, 0, Some(IssuesBasis::Validated)) => ("✓ Valid".into(), NSColor::systemGreenColor()),
        (0, w, Some(IssuesBasis::Validated)) => (count(w, "warning"), NSColor::systemOrangeColor()),
        (e, 0, _) => (
            format!("⚠ {}", count(e, "error")),
            NSColor::systemRedColor(),
        ),
        (e, w, _) => (
            format!("⚠ {}, {}", count(e, "error"), count(w, "warning")),
            NSColor::systemRedColor(),
        ),
    }
}

/// "Today 14:03:12", "Yesterday 17:02:10", else a short date, in the user's locale.
fn sent_formatter() -> Retained<NSDateFormatter> {
    let formatter = NSDateFormatter::new();
    formatter.setDateStyle(NSDateFormatterStyle::ShortStyle);
    formatter.setTimeStyle(NSDateFormatterStyle::MediumStyle);
    formatter.setDoesRelativeDateFormatting(true);
    formatter
}

/// `time` in `formatter`'s style. The response pane and history show when a request was
/// sent: the received time differs only by the duration shown beside it, and a failed send
/// has none.
pub(crate) fn date_text(formatter: &NSDateFormatter, time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let date = NSDate::dateWithTimeIntervalSince1970(since_epoch.as_secs_f64());
    formatter.stringFromDate(&date).to_string()
}

/// `200 · 120 ms · 1.2 KB`, `500 · SOAP Fault: soapenv:Server: no such customer`, or
/// `Failed: connection refused`.
fn status_line(response: &ResponseView) -> String {
    if let Some(error) = &response.error {
        return format!("Failed: {error}");
    }
    let mut parts = vec![
        response
            .status
            .map_or_else(|| "—".to_owned(), |s| s.to_string()),
    ];
    if let Some(duration) = response.duration {
        parts.push(format!("{} ms", duration.as_millis()));
    }
    parts.push(format!("{:.1} KB", response.size as f64 / 1024.0));
    if let Some(fault) = &response.fault {
        parts.push(format!("SOAP Fault: {}: {}", fault.code, fault.string));
    }
    parts.join(" · ")
}

impl ModelAccess for ResponsePane {}
