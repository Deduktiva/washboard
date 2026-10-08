//! The issues bar under the editor and the response pane below it (PLAN §8).

use std::cell::{OnceCell, RefCell};
use std::time::UNIX_EPOCH;

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSFont, NSLayoutAttribute, NSScrollView, NSStackView, NSTabView,
    NSTabViewItem, NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    NSArray, NSDate, NSDateFormatter, NSDateFormatterStyle, NSObject, NSObjectProtocol, NSString,
    ns_string,
};
use washboard_core::model::HistoryId;
use washboard_ui_model::{App, Issue, ModelError, ProjectKey, ResponseView};

use crate::app::with_delegate;
use crate::editor::EditorController;
use crate::table::TextTable;

#[derive(Debug)]
pub struct IssuesIvars {
    editor: Retained<EditorController>,
    table: Retained<TextTable>,
    /// The listed issues, in the table's order.
    issues: RefCell<Vec<Issue>>,
    summary: OnceCell<Retained<NSTextField>>,
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
            list.setHidden(!list.isHidden());
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
            view: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let summary = NSTextField::labelWithString(ns_string!(""), mtm);
        summary.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        summary.setTextColor(Some(&NSColor::systemOrangeColor()));
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
        let header: [Retained<NSView>; 2] = [
            Retained::into_super(Retained::into_super(summary.clone())),
            Retained::into_super(Retained::into_super(hide)),
        ];
        let header = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&header), mtm);
        header.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);

        // The table belongs to this bar: a strong reference back would be a cycle.
        let bar = Weak::from(&*this);
        this.ivars().table.on_click(move |row| {
            if let Some(bar) = bar.load() {
                bar.reveal_issue(row);
            }
        });
        let list: Retained<NSView> = Retained::into_super(this.ivars().table.view().retain());
        let views = [Retained::into_super(header), list];
        let view = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        view.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        view.setAlignment(NSLayoutAttribute::Leading);
        let _ = this.ivars().summary.set(summary);
        let _ = this.ivars().view.set(view);
        this
    }

    pub fn view(&self) -> &NSStackView {
        self.ivars().view.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        &self.ivars().table
    }

    /// Lists the editor's issues, marks their lines in the ruler and underlines their ranges
    /// (`DiagnosticsChanged`).
    pub fn set_issues(&self, issues: Vec<Issue>) {
        let errors = issues.iter().filter(|i| i.is_error()).count();
        let warnings = issues.len() - errors;
        let plural = |n: usize, what: &str| match n {
            1 => format!("1 {what}"),
            n => format!("{n} {what}s"),
        };
        let summary = match (errors, warnings) {
            (0, 0) => "No issues".to_owned(),
            (e, 0) => format!("⚠ {}", plural(e, "error")),
            (0, w) => plural(w, "warning"),
            (e, w) => format!("⚠ {}, {}", plural(e, "error"), plural(w, "warning")),
        };
        if let Some(label) = self.ivars().summary.get() {
            label.setStringValue(&NSString::from_str(&summary));
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
        self.ivars().table.view().setHidden(false);
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
        let headers = NSTextView::scrollableTextView(mtm);
        headers.setAutohidesScrollers(true);
        if let Some(text) = headers
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
        {
            text.setEditable(false);
        }
        let history = TextTable::new(&["Sent", "Status", "Time"], mtm);
        let tabs = NSTabView::new(mtm);
        let status = NSTextField::labelWithString(ns_string!("No response yet"), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(11.0)));

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
        let history_page: [Retained<NSView>; 2] = [
            Retained::into_super(this.ivars().history.view().retain()),
            Retained::into_super(Retained::into_super(restore)),
        ];
        let history_page =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&history_page), mtm);
        history_page.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        history_page.setAlignment(NSLayoutAttribute::Leading);

        let pages: [Retained<NSView>; 3] = [
            Retained::into_super(this.ivars().body.view().retain()),
            Retained::into_super(this.ivars().headers.clone()),
            Retained::into_super(history_page),
        ];
        for (label, page) in RESPONSE_TABS.iter().zip(&pages) {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(page));
            this.ivars().tabs.addTabViewItem(&item);
        }
        let views: [Retained<NSView>; 2] = [
            Retained::into_super(Retained::into_super(this.ivars().status.clone())),
            Retained::into_super(this.ivars().tabs.clone()),
        ];
        let view = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        view.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        view.setAlignment(NSLayoutAttribute::Leading);
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
        self.ivars()
            .headers
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
            .map(|t| t.string().to_string())
            .unwrap_or_default()
    }

    pub fn history(&self) -> &TextTable {
        &self.ivars().history
    }

    /// Shows the model's response for the window (`ResponseChanged`, `SendStateChanged`).
    pub fn show_response(&self) {
        let key = self.ivars().key;
        let Some((response, sending)) = self
            .read(|app| {
                let window = app.project(key)?;
                Some((window.response().cloned(), window.sending()))
            })
            .flatten()
        else {
            return;
        };
        let status = match (&response, sending) {
            (_, true) => "Sending…".to_owned(),
            (None, false) => "No response yet".to_owned(),
            (Some(response), false) => status_line(response),
        };
        self.set_status(&status);
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
        if let Some(text) = self
            .ivars()
            .headers
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
        {
            text.setString(&NSString::from_str(&headers));
        }
    }

    /// Lists the selected request's history (`HistoryChanged`).
    pub fn show_history(&self) {
        let key = self.ivars().key;
        let entries = self
            .read(|app| app.project(key).map(|w| w.history().to_vec()))
            .flatten()
            .unwrap_or_default();
        let formatter = NSDateFormatter::new();
        formatter.setDateStyle(NSDateFormatterStyle::ShortStyle);
        formatter.setTimeStyle(NSDateFormatterStyle::MediumStyle);
        let rows = entries
            .iter()
            .map(|e| {
                let since_epoch = e.sent_at.duration_since(UNIX_EPOCH).unwrap_or_default();
                let date = NSDate::dateWithTimeIntervalSince1970(since_epoch.as_secs_f64());
                let status = match (&e.error, e.http_status) {
                    (Some(_), _) => "Failed".to_owned(),
                    (None, Some(code)) if e.soap_fault => format!("{code} Fault"),
                    (None, Some(code)) => code.to_string(),
                    (None, None) => "—".to_owned(),
                };
                let time = e
                    .duration
                    .map_or_else(String::new, |d| format!("{} ms", d.as_millis()));
                vec![formatter.stringFromDate(&date).to_string(), status, time]
            })
            .collect();
        self.ivars().history.set_rows(rows);
        *self.ivars().history_ids.borrow_mut() = entries.iter().map(|e| e.id).collect();
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

    fn read<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        with_delegate(self.mtm(), |d| d.read(f)).flatten()
    }

    fn command<R>(&self, title: &str, f: impl FnOnce(&mut App) -> Result<R, ModelError>) {
        with_delegate(self.mtm(), |d| d.command(title, f));
    }
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
