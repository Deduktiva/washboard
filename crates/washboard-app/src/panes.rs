//! The issues bar under the editor and the response pane below it (PLAN §8).

use std::cell::{OnceCell, RefCell};

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSFont, NSLayoutAttribute, NSScrollView, NSStackView, NSTabView,
    NSTabViewItem, NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, ns_string};

use washboard_ui_model::Issue;

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
    status: Retained<NSTextField>,
    body: Retained<EditorController>,
    headers: Retained<NSScrollView>,
    history: Retained<TextTable>,
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
);

impl ResponsePane {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let body = EditorController::new(mtm);
        body.text_view().setEditable(false);
        let headers = NSTextView::scrollableTextView(mtm);
        if let Some(text) = headers
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
        {
            text.setEditable(false);
        }
        let history = TextTable::new(&["Sent", "Status", "Time"], mtm);
        let tabs = NSTabView::new(mtm);
        let pages: [Retained<NSView>; 3] = [
            Retained::into_super(body.view().retain()),
            Retained::into_super(headers.clone()),
            Retained::into_super(history.view().retain()),
        ];
        for (label, page) in RESPONSE_TABS.iter().zip(&pages) {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(page));
            tabs.addTabViewItem(&item);
        }
        let status = NSTextField::labelWithString(ns_string!("No response yet"), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(11.0)));

        let this = Self::alloc(mtm).set_ivars(ResponseIvars {
            status,
            body,
            headers,
            history,
            tabs,
            view: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
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

    pub fn history(&self) -> &TextTable {
        &self.ivars().history
    }

    /// Shows a finished exchange: status line, body, headers and a new history row.
    pub fn show(&self, response: &FakeResponse) {
        self.set_status(&response.status_line());
        self.ivars().body.set_text(&response.body);
        if let Some(text) = self
            .ivars()
            .headers
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
        {
            text.setString(&NSString::from_str(&response.headers));
        }
        let mut rows = self.ivars().history.rows();
        rows.insert(
            0,
            vec![
                response.sent.clone(),
                response.status.to_string(),
                format!("{} ms", response.millis),
            ],
        );
        self.ivars().history.set_rows(rows);
    }
}

/// What the fake Send produces until `washboard_core::http` is wired in (WP-APP-INTEGRATION).
#[derive(Debug, Clone)]
pub struct FakeResponse {
    pub sent: String,
    pub status: u16,
    pub reason: String,
    pub millis: u64,
    pub headers: String,
    pub body: String,
}

impl FakeResponse {
    pub fn status_line(&self) -> String {
        format!(
            "{} {} · {} ms · {:.1} KB",
            self.status,
            self.reason,
            self.millis,
            self.body.len() as f64 / 1024.0
        )
    }
}
