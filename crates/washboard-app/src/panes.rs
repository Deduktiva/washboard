//! The request bar above the editor with the validity widget, the issues list under it, and the
//! response pane beside them with its History drawer (`docs/specs/response-pane.md`).

use std::cell::{Cell, OnceCell, RefCell};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSClickGestureRecognizer, NSColor, NSControlSize, NSEvent, NSFont,
    NSImage, NSImageView, NSLayoutConstraintOrientation, NSLayoutPriorityDefaultLow,
    NSLineBreakMode, NSProgressIndicator, NSProgressIndicatorStyle, NSResponder, NSScrollView,
    NSSplitView, NSSplitViewController, NSSplitViewDividerStyle, NSSplitViewItem, NSStackView,
    NSStackViewGravity, NSTabView, NSTabViewItem, NSTextField, NSTitlePosition, NSView,
    NSViewController,
};
use objc2_foundation::{
    NSArray, NSByteCountFormatter, NSByteCountFormatterCountStyle, NSDate, NSDateFormatter,
    NSDateFormatterStyle, NSObject, NSObjectProtocol, NSSize, NSString, ns_string,
};
use washboard_core::model::HistoryId;
use washboard_ui_model::{
    HistoryDrawer, Issue, ProjectKey, RequestSummary, ResponseView, Validity,
};

use crate::app::ModelAccess;
use crate::colors;
use crate::editor::EditorController;
use crate::layout;
use crate::table::TextTable;
use crate::text::{count, operation_chip};
use crate::views::{read_only_text, read_only_text_get_text, read_only_text_set_text};

/// The bar above the editor (`docs/gui-draft.html`): the request's name, a chip with its
/// operation, and at its right end the validity widget (`IssuesBar`), or "Sent … · read-only"
/// while an older exchange is shown. Plain views with no actions, so no Objective-C class of its
/// own.
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
        state.setFont(Some(&NSFont::systemFontOfSize(layout::STATUS_FONT_SIZE)));
        state.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let view = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        view.addView_inGravity(&name, NSStackViewGravity::Leading);
        view.addView_inGravity(&operation, NSStackViewGravity::Leading);
        view.addView_inGravity(&state, NSStackViewGravity::Trailing);
        view.setSpacing(10.0);
        view.setEdgeInsets(layout::insets(0.0, 12.0, 0.0, 12.0));
        layout::hug_vertically(&view);
        layout::set_height(&view, layout::BAR_HEIGHT);
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
    }

    /// `Some("Sent … · read-only")` while an older exchange's request is shown in place of
    /// the editor; `None` clears it.
    pub fn show_sent(&self, sent: Option<&str>) {
        self.state
            .setStringValue(&NSString::from_str(sent.unwrap_or_default()));
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

    /// A view at the right end, beside the "Sent … · read-only" label: the validity widget.
    pub fn add_trailing(&self, view: &NSView) {
        self.view
            .addView_inGravity(view, NSStackViewGravity::Trailing);
    }

    /// The trailing label: "Sent … · read-only", or empty.
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
    /// The validity widget at the right end of the request bar, and its button.
    widget: OnceCell<Retained<NSBox>>,
    /// A request is open, so the widget has something to say.
    open: Cell<bool>,
    /// An older exchange's request is shown in place of the editor, so the widget gives way.
    sent: Cell<bool>,
    summary: OnceCell<Retained<NSButton>>,
    /// The hairline above the list, shown and hidden with it.
    line: OnceCell<Retained<NSBox>>,
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
            self.show_list(!self.list_shown());
        }
    }
);

impl IssuesBar {
    pub fn new(editor: &EditorController, mtm: MainThreadMarker) -> Retained<Self> {
        // Line and message, without a header: the list is only a few rows high.
        let table = TextTable::new(&["", ""], mtm);
        table.fix_column_width(0, LINE_COLUMN_WIDTH);
        table.wrap_column(1);
        let this = Self::alloc(mtm).set_ivars(IssuesIvars {
            editor: editor.retain(),
            table,
            issues: RefCell::new(Vec::new()),
            widget: OnceCell::new(),
            open: Cell::new(false),
            sent: Cell::new(false),
            summary: OnceCell::new(),
            line: OnceCell::new(),
            view: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        // The request bar shows it (`RequestBar::add_trailing`).
        let (widget, summary) = validity_widget(&this, mtm);
        widget.setHidden(true);

        // The table belongs to this bar: a strong reference back would be a cycle.
        let bar = Weak::from(&*this);
        this.ivars().table.on_click(move |row| {
            if let Some(bar) = bar.load() {
                bar.reveal_issue(row);
            }
        });
        let list: Retained<NSView> = Retained::into_super(this.ivars().table.view().retain());
        layout::set_height(&list, ISSUES_HEIGHT);
        let line = NSBox::new(mtm);
        line.setBoxType(NSBoxType::Separator);
        let view = layout::fill_column(&[Retained::into_super(line.clone()), list], mtm);
        layout::hug_vertically(&view);
        let _ = this.ivars().widget.set(widget);
        let _ = this.ivars().summary.set(summary);
        let _ = this.ivars().line.set(line);
        let _ = this.ivars().view.set(view);
        this
    }

    pub fn view(&self) -> &NSStackView {
        self.ivars().view.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        &self.ivars().table
    }

    /// The validity widget's text: the validation state of the editor's text.
    pub fn summary(&self) -> String {
        self.ivars()
            .summary
            .get()
            .map(|button| button.title().to_string())
            .unwrap_or_default()
    }

    /// The validity widget, for layout checks.
    pub fn widget(&self) -> &NSBox {
        self.ivars().widget.get().expect("set in new()")
    }

    /// The validity widget's button, which shows or hides the list.
    pub fn widget_button(&self) -> &NSButton {
        self.ivars().summary.get().expect("set in new()")
    }

    /// Lists the editor's issues, marks their lines in the ruler, underlines their ranges and
    /// sums them up in the validity widget (`DiagnosticsChanged`). `validity` is `None` with no
    /// request open, which hides the widget.
    pub fn set_issues(&self, issues: Vec<Issue>, validity: Option<Validity>) {
        let (summary, colour) = summary(validity);
        if let Some(button) = self.ivars().summary.get() {
            button.setTitle(&NSString::from_str(&summary));
            button.setContentTintColor(Some(&colour));
        }
        self.ivars().open.set(validity.is_some());
        self.show_widget();
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
        editor.ruler().set_issues(&issues);
        editor.set_underlines(
            issues
                .iter()
                .filter(|i| i.is_error())
                .filter_map(|i| Some((i.range.clone()?, i.message.clone())))
                .collect(),
        );
        *self.ivars().issues.borrow_mut() = issues;
    }

    /// While an older exchange is shown, the request bar is about the sent request: the widget
    /// gives way to its "Sent … · read-only" label.
    pub fn show_sent(&self, sent: bool) {
        self.ivars().sent.set(sent);
        self.show_widget();
    }

    fn show_widget(&self) {
        let ivars = self.ivars();
        self.widget()
            .setHidden(!ivars.open.get() || ivars.sent.get());
    }

    /// Shows the list if it was hidden (`ShowIssues`).
    pub fn reveal(&self) {
        self.show_list(true);
    }

    /// A hidden list leaves the stack, so the editor takes its height. The list's own views
    /// are hidden, not the bar: the bar is hidden while an older exchange is shown, and showing
    /// it again must not undo the user's choice.
    fn show_list(&self, shown: bool) {
        self.ivars().table.view().setHidden(!shown);
        if let Some(line) = self.ivars().line.get() {
            line.setHidden(!shown);
        }
    }

    fn list_shown(&self) -> bool {
        !self.ivars().table.view().isHidden()
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
/// The issues list's line column: four digits and "—".
const LINE_COLUMN_WIDTH: f64 = 44.0;

pub const RESPONSE_TABS: [&str; 2] = ["Response", "Headers"];

type DragEnd = Box<dyn Fn()>;

#[derive(Default)]
pub struct DrawerSplitIvars {
    /// Called when a drag of the divider ends.
    on_drag_end: RefCell<Option<DragEnd>>,
    /// The History table's height to apply once the split has a size of its own.
    pending_height: Cell<Option<f64>>,
}

impl fmt::Debug for DrawerSplitIvars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrawerSplitIvars")
            .field("pending_height", &self.pending_height)
            .finish_non_exhaustive()
    }
}

define_class!(
    // SAFETY:
    // - NSSplitView has no subclassing requirements.
    // - `DrawerSplit` does not implement `Drop`.
    #[unsafe(super(NSSplitView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DrawerSplitIvars]
    #[derive(Debug)]
    pub struct DrawerSplit;

    impl DrawerSplit {
        // SAFETY: the signature matches `mouseDown:`.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            // NSSplitView tracks a divider drag inside `mouseDown:` and returns on mouse up, so
            // the drawer's height is saved once per drag rather than on every step of it.
            // SAFETY: calling the superclass's implementation with its own arguments.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            if let Some(f) = &*self.ivars().on_drag_end.borrow() {
                f();
            }
        }

        // SAFETY: the signature matches `layout`.
        #[unsafe(method(layout))]
        fn layout(&self) {
            // SAFETY: calling the superclass's implementation.
            let _: () = unsafe { msg_send![super(self), layout] };
            let total = self.frame().size.height;
            if let Some(height) = self.ivars().pending_height.get()
                && total > 0.0
            {
                self.ivars().pending_height.set(None);
                let position = (total - height - self.dividerThickness()).max(0.0);
                self.setPosition_ofDividerAtIndex(position, 0);
            }
        }
    }
);

impl DrawerSplit {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DrawerSplitIvars::default());
        // SAFETY: `NSView`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // Horizontal dividers: the response above, the History table below.
        this.setVertical(false);
        this.setDividerStyle(NSSplitViewDividerStyle::Thin);
        this
    }
}

#[derive(Debug)]
pub struct ResponseIvars {
    key: ProjectKey,
    status: Retained<NSTextField>,
    /// Spins beside the status while a send is in flight.
    spinner: Retained<NSProgressIndicator>,
    body: Retained<EditorController>,
    headers: Retained<NSScrollView>,
    history: Retained<TextTable>,
    /// The history rows' entries, in the table's order (newest first).
    history_ids: RefCell<Vec<HistoryId>>,
    tabs: Retained<NSTabView>,
    disclosure: Retained<NSImageView>,
    /// "3 earlier exchanges".
    earlier: Retained<NSTextField>,
    /// One dot per stored exchange, oldest left.
    dots: Retained<NSStackView>,
    drawer_split: Retained<DrawerSplit>,
    drawer: OnceCell<Retained<NSSplitViewController>>,
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
        #[unsafe(method(toggleHistory:))]
        fn toggle_history(&self, _sender: Option<&AnyObject>) {
            self.set_drawer_open(!self.drawer_open());
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
        let disclosure = NSImageView::new(mtm);
        let earlier = layout::small_label("", mtm);
        earlier.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let dots = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        dots.setSpacing(3.0);

        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setIndeterminate(true);
        spinner.setHidden(true);

        let this = Self::alloc(mtm).set_ivars(ResponseIvars {
            key,
            status,
            spinner,
            body,
            headers,
            history,
            history_ids: RefCell::new(Vec::new()),
            tabs,
            disclosure,
            earlier,
            dots,
            drawer_split: DrawerSplit::new(mtm),
            drawer: OnceCell::new(),
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
        let pane = Weak::from(&*this);
        *this.ivars().drawer_split.ivars().on_drag_end.borrow_mut() = Some(Box::new(move || {
            if let Some(pane) = pane.load() {
                pane.drawer_dragged();
            }
        }));

        let pages: [Retained<NSView>; 2] = [
            Retained::into_super(this.ivars().body.view().retain()),
            Retained::into_super(this.ivars().headers.clone()),
        ];
        for (label, page) in RESPONSE_TABS.iter().zip(&pages) {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(&layout::tab_page(page, mtm)));
            this.ivars().tabs.addTabViewItem(&item);
        }

        // The drawer's header closes the response half, so it stays in view with the table
        // collapsed; the table is the split's second half.
        let header = this.drawer_header(mtm);
        let response_vc = NSViewController::new(mtm);
        // A hairline sets the header off from the response body, as the split's divider does
        // from the table below.
        let line = NSBox::new(mtm);
        line.setBoxType(NSBoxType::Separator);
        response_vc.setView(&layout::fill_column(
            &[
                Retained::into_super(this.ivars().tabs.clone()),
                Retained::into_super(line),
                Retained::into_super(header),
            ],
            mtm,
        ));
        let table_vc = NSViewController::new(mtm);
        table_vc.setView(this.ivars().history.view());
        let drawer = NSSplitViewController::new(mtm);
        drawer.setSplitView(&this.ivars().drawer_split);
        let response_item = NSSplitViewItem::splitViewItemWithViewController(&response_vc);
        response_item.setMinimumThickness(MIN_BODY_HEIGHT);
        drawer.addSplitViewItem(&response_item);
        let table_item = NSSplitViewItem::splitViewItemWithViewController(&table_vc);
        table_item.setMinimumThickness(MIN_HISTORY_HEIGHT);
        table_item.setCanCollapse(true);
        // A taller window gives its height to the response, not to the table.
        table_item.setHoldingPriority(NSLayoutPriorityDefaultLow + 10.0);
        table_item.setCollapsed(true);
        drawer.addSplitViewItem(&table_item);

        let status = layout::row(
            &[
                layout::view(this.ivars().spinner.clone()),
                layout::view(this.ivars().status.clone()),
            ],
            mtm,
        );
        // The request bar's insets, so both halves' text starts as far from its edge.
        status.setEdgeInsets(layout::insets(0.0, 12.0, 0.0, 12.0));
        layout::set_height(&status, layout::BAR_HEIGHT);
        let view = layout::fill_column(&[Retained::into_super(status), drawer.view()], mtm);
        let _ = this.ivars().drawer.set(drawer);
        let _ = this.ivars().view.set(view);
        this.show_disclosure(false);
        this
    }

    /// The History drawer's header line: disclosure, title, count and dots. A click anywhere on
    /// it opens or closes the table.
    fn drawer_header(&self, mtm: MainThreadMarker) -> Retained<NSStackView> {
        let title = NSTextField::labelWithString(ns_string!("History"), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(
            layout::STATUS_FONT_SIZE,
        )));
        let header = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        header.addView_inGravity(&self.ivars().disclosure, NSStackViewGravity::Leading);
        header.addView_inGravity(&title, NSStackViewGravity::Leading);
        header.addView_inGravity(&self.ivars().earlier, NSStackViewGravity::Leading);
        header.addView_inGravity(&self.ivars().dots, NSStackViewGravity::Trailing);
        header.setSpacing(8.0);
        header.setEdgeInsets(layout::insets(0.0, 12.0, 0.0, 12.0));
        layout::set_height(&header, DRAWER_HEADER_HEIGHT);
        // SAFETY: this pane owns the header (through its view) and so the recognizer, which
        // holds its target weakly; `toggleHistory:` takes the sender.
        let click = unsafe {
            NSClickGestureRecognizer::initWithTarget_action(
                NSClickGestureRecognizer::alloc(mtm),
                Some(self),
                Some(sel!(toggleHistory:)),
            )
        };
        header.addGestureRecognizer(&click);
        header.setToolTip(Some(ns_string!("Show or hide the request's history")));
        header
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

    /// Whether the spinner beside the status is shown.
    pub fn is_spinning(&self) -> bool {
        !self.ivars().spinner.isHidden()
    }

    /// A hidden spinner leaves the status row, so the status keeps its place when idle.
    fn show_spinner(&self, sending: bool) {
        let spinner = &self.ivars().spinner;
        spinner.setHidden(!sending);
        // SAFETY: both take any sender, and nil is allowed.
        unsafe {
            if sending {
                spinner.startAnimation(None);
            } else {
                spinner.stopAnimation(None);
            }
        }
    }

    pub fn tabs(&self) -> &NSTabView {
        &self.ivars().tabs
    }

    pub fn body(&self) -> &EditorController {
        &self.ivars().body
    }

    /// The Headers tab's text view.
    pub fn headers_view(&self) -> &NSScrollView {
        &self.ivars().headers
    }

    /// The Headers tab's text.
    pub fn headers(&self) -> String {
        read_only_text_get_text(&self.ivars().headers)
    }

    pub fn history(&self) -> &TextTable {
        &self.ivars().history
    }

    /// The drawer header's "3 earlier exchanges".
    pub fn earlier_exchanges(&self) -> String {
        self.ivars().earlier.stringValue().to_string()
    }

    /// The drawer header's dots, oldest first: whether each is red and whether it is ringed.
    pub fn dots(&self) -> Vec<(bool, bool)> {
        self.ivars()
            .dots
            .arrangedSubviews()
            .iter()
            .filter_map(|v| v.downcast::<NSBox>().ok())
            .map(|dot| {
                let red = dot.fillColor() == NSColor::systemRedColor();
                (red, dot.borderWidth() > 0.0)
            })
            .collect()
    }

    /// The split of response and History table, for the window controller to own.
    pub fn drawer(&self) -> &NSSplitViewController {
        self.ivars().drawer.get().expect("set in new()")
    }

    fn table_item(&self) -> Option<Retained<NSSplitViewItem>> {
        self.drawer().splitViewItems().iter().nth(1)
    }

    /// The History table is shown.
    pub fn drawer_open(&self) -> bool {
        self.table_item().is_some_and(|item| !item.isCollapsed())
    }

    /// Opens or closes the History table, as a click on the drawer's header does, and
    /// remembers it for the project.
    pub fn set_drawer_open(&self, open: bool) {
        self.show_drawer(self.save_drawer(open));
    }

    /// Shows the drawer as the model remembers it (when the window opens).
    pub fn show_model_drawer(&self) {
        self.show_drawer(self.model_drawer());
    }

    fn model_drawer(&self) -> HistoryDrawer {
        let key = self.ivars().key;
        self.read_project(key, |w| w.history_drawer())
            .unwrap_or_default()
    }

    /// Remembers the drawer as `open`, with the table's height if it is shown now.
    fn save_drawer(&self, open: bool) -> HistoryDrawer {
        let mut drawer = self.model_drawer();
        if self.drawer_open() {
            drawer.height = Some(self.ivars().history.view().frame().size.height);
        }
        drawer.open = open;
        let key = self.ivars().key;
        self.command("Could not save the History drawer", |app| {
            app.set_history_drawer(key, drawer)
        });
        drawer
    }

    fn show_drawer(&self, drawer: HistoryDrawer) {
        if let Some(item) = self.table_item() {
            item.setCollapsed(!drawer.open);
        }
        if drawer.open {
            // Uncollapsed with no height of its own, the split gives the table its minimum:
            // the header and one row. Earlier versions saved that as the drawer's height, so a
            // height at the minimum is taken as unset too.
            let height = drawer
                .height
                .filter(|&h| h > MIN_HISTORY_HEIGHT)
                .unwrap_or(DEFAULT_HISTORY_HEIGHT);
            let split = &self.ivars().drawer_split;
            split.ivars().pending_height.set(Some(height));
            split.setNeedsLayout(true);
        }
        self.show_disclosure(drawer.open);
    }

    fn show_disclosure(&self, open: bool) {
        let (symbol, label) = if open {
            ("chevron.down", "Hide History")
        } else {
            ("chevron.right", "Show History")
        };
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&NSString::from_str(label)),
        );
        self.ivars().disclosure.setImage(image.as_deref());
    }

    /// After the user dragged the divider: the table's height, or that it was dragged shut.
    fn drawer_dragged(&self) {
        let drawer = self.save_drawer(self.drawer_open());
        self.show_disclosure(drawer.open);
    }

    /// Shows the model's response for the window (`ResponseChanged`, `SendStateChanged`).
    pub fn show_response(&self) {
        let key = self.ivars().key;
        let Some((response, server, sending)) = self.read_project(key, |window| {
            let response = window.response().cloned();
            let server = response
                .as_ref()
                .map(|r| window.exchange_label(r.server, &r.url, r.parameter_set));
            (response, server, window.sending())
        }) else {
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
        self.show_spinner(sending);
        // The full URL, for when the server's name is not enough.
        let url = response.as_ref().map(|r| NSString::from_str(&r.url));
        self.ivars().status.setToolTip(url.as_deref());
        let body = response
            .as_ref()
            .and_then(|r| r.body.clone())
            .unwrap_or_default();
        self.ivars().body.set_text(&body);
        let headers = response.as_ref().map(ResponseView::headers_text);
        read_only_text_set_text(&self.ivars().headers, &headers.unwrap_or_default());
    }

    /// Lists the selected request's history, with the shown exchange selected and ringed
    /// (`HistoryChanged`, `ShownExchangeChanged`).
    pub fn show_history(&self) {
        let key = self.ivars().key;
        let (entries, older) = self
            .read_project(key, |window| {
                let entries = window.history().iter().map(|e| {
                    let server = window.exchange_label(e.server_id, &e.url, e.parameter_set_id);
                    (e.clone(), server)
                });
                let older = window.older_exchange().map(|o| o.entry);
                (entries.collect::<Vec<_>>(), older)
            })
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

        // Newest first in the table; the latest is shown unless an older one is.
        let shown = older
            .and_then(|id| entries.iter().position(|(e, _)| e.id == id))
            .unwrap_or(0);
        // Through `select`, which the table does not report as the user's pick: reporting it
        // would show that exchange again and come back here. Reloaded rows start unselected.
        if !entries.is_empty() {
            self.ivars().history.select(shown);
        }
        let earlier = entries.len().saturating_sub(1);
        let earlier = if earlier == 0 {
            "no earlier exchanges".to_owned()
        } else {
            count(earlier, "earlier exchange")
        };
        self.ivars()
            .earlier
            .setStringValue(&NSString::from_str(&earlier));

        let dots = &self.ivars().dots;
        for dot in dots.arrangedSubviews().iter() {
            dots.removeView(&dot);
        }
        let mtm = self.mtm();
        for (i, (e, _)) in entries.iter().enumerate().rev() {
            let failed = e.error.is_some() || e.soap_fault;
            let dot = history_dot(failed, i == shown, mtm);
            dot.setToolTip(Some(&NSString::from_str(&date_text(&formatter, e.sent_at))));
            dots.addView_inGravity(&dot, NSStackViewGravity::Trailing);
        }
    }

    /// A click on a history row shows that exchange; the newest returns to the latest.
    pub fn show_history_row(&self, row: usize) {
        let Some(entry) = self.ivars().history_ids.borrow().get(row).copied() else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not show the history entry", |app| {
            app.show_history(key, entry)
        });
    }
}

/// The History drawer's header line, and the least each half of the drawer keeps.
const DRAWER_HEADER_HEIGHT: f64 = 26.0;
const MIN_BODY_HEIGHT: f64 = 120.0;
const MIN_HISTORY_HEIGHT: f64 = 60.0;
/// The History table's height until the user drags it: its header and about five rows.
const DEFAULT_HISTORY_HEIGHT: f64 = 150.0;
const DOT_SIZE: f64 = 7.0;

/// One stored exchange in the drawer's header: green, red for a SOAP Fault or a transport
/// failure, ringed in the accent colour while shown. An `NSBox` rather than a drawn view, so
/// its semantic colours follow the appearance.
fn history_dot(failed: bool, shown: bool, mtm: MainThreadMarker) -> Retained<NSBox> {
    let dot = NSBox::new(mtm);
    dot.setBoxType(NSBoxType::Custom);
    dot.setTitlePosition(NSTitlePosition::NoTitle);
    dot.setContentViewMargins(NSSize::new(0.0, 0.0));
    let fill = if failed {
        NSColor::systemRedColor()
    } else {
        NSColor::systemGreenColor()
    };
    dot.setFillColor(&fill);
    dot.setCornerRadius(DOT_SIZE / 2.0);
    dot.setBorderColor(&NSColor::controlAccentColor());
    dot.setBorderWidth(if shown { 1.5 } else { 0.0 });
    for constraint in [
        dot.widthAnchor().constraintEqualToConstant(DOT_SIZE),
        dot.heightAnchor().constraintEqualToConstant(DOT_SIZE),
    ] {
        constraint.setActive(true);
    }
    dot
}

/// The validity widget: a borderless button that toggles the issues list, in a pill-shaped
/// outline like the operation chip beside it.
fn validity_widget(
    target: &IssuesBar,
    mtm: MainThreadMarker,
) -> (Retained<NSBox>, Retained<NSButton>) {
    let button = layout::button("", target, sel!(toggleIssues:), mtm);
    button.setBordered(false);
    button.setFont(Some(&NSFont::systemFontOfSize(layout::STATUS_FONT_SIZE)));
    button.setToolTip(Some(ns_string!("Show or hide the issues list")));
    let plate = NSBox::new(mtm);
    plate.setBoxType(NSBoxType::Custom);
    plate.setTitlePosition(NSTitlePosition::NoTitle);
    // Semantic colours, so the outline follows the appearance (a CALayer border would not).
    plate.setBorderColor(&NSColor::separatorColor());
    plate.setBorderWidth(1.0);
    plate.setCornerRadius(WIDGET_HEIGHT / 2.0);
    plate.setContentViewMargins(NSSize::new(0.0, 0.0));
    if let Some(content) = plate.contentView() {
        button.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&button);
        for constraint in [
            button
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), WIDGET_PADDING),
            content
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&button.trailingAnchor(), WIDGET_PADDING),
            button
                .centerYAnchor()
                .constraintEqualToAnchor(&content.centerYAnchor()),
            plate
                .heightAnchor()
                .constraintEqualToConstant(WIDGET_HEIGHT),
        ] {
            constraint.setActive(true);
        }
    }
    (plate, button)
}

/// The validity widget's height; it is a pill.
const WIDGET_HEIGHT: f64 = 20.0;
const WIDGET_PADDING: f64 = 8.0;

/// The validity widget's text and colour; empty with no request open.
fn summary(validity: Option<Validity>) -> (String, Retained<NSColor>) {
    match validity {
        None => (String::new(), NSColor::secondaryLabelColor()),
        Some(Validity::Checking) => ("Checking…".into(), NSColor::secondaryLabelColor()),
        Some(Validity::SchemaNotChecked) => {
            ("Well-formed · schema not checked".into(), colors::warning())
        }
        Some(Validity::Valid) => ("✓ Valid".into(), colors::success()),
        Some(Validity::Warnings(w)) => (count(w, "warning"), colors::warning()),
        Some(Validity::Errors {
            errors,
            warnings: 0,
        }) => (format!("⚠ {}", count(errors, "error")), colors::error()),
        Some(Validity::Errors { errors, warnings }) => (
            format!(
                "⚠ {}, {}",
                count(errors, "error"),
                count(warnings, "warning")
            ),
            colors::error(),
        ),
    }
}

/// "Today 14:03:12", "Yesterday 17:02:10", else a short date, in the user's locale.
pub(crate) fn sent_formatter() -> Retained<NSDateFormatter> {
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

/// `200 · 120 ms · 1 KB`, `500 · SOAP Fault: soapenv:Server: no such customer`, or
/// `Failed: connection refused`. The size is in the user's locale and Finder's units.
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
    let size = i64::try_from(response.size).unwrap_or(i64::MAX);
    parts.push(
        NSByteCountFormatter::stringFromByteCount_countStyle(
            size,
            NSByteCountFormatterCountStyle::File,
        )
        .to_string(),
    );
    if let Some(fault) = &response.fault {
        parts.push(format!("SOAP Fault: {}: {}", fault.code, fault.string));
    }
    parts.join(" · ")
}

impl ModelAccess for ResponsePane {}
