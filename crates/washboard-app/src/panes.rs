//! The request bar above the editor, the issues bar under it, and the response pane beside
//! them with its History drawer (PLAN §4 "Response pane and history", §8).

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
    HistoryDrawer, Issue, IssuesBasis, ProjectKey, RequestSummary, ResponseView, WellFormedness,
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
    /// The editor's live state, shown again when the sent-request label goes.
    well_formedness: Cell<WellFormedness>,
    /// "Sent <time> · read-only" while an older exchange is shown.
    sent: RefCell<Option<String>>,
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
            well_formedness: Cell::new(WellFormedness::Pending),
            sent: RefCell::new(None),
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

    /// The well-formedness state alone, which changes with every check. While an older
    /// exchange is shown it is kept for later: the bar is about the sent request then.
    pub fn set_state(&self, state: WellFormedness) {
        self.well_formedness.set(state);
        if self.sent.borrow().is_some() {
            return;
        }
        self.state
            .setStringValue(&NSString::from_str(&well_formedness_text(state)));
        let colour = match state {
            WellFormedness::Pending => NSColor::secondaryLabelColor(),
            WellFormedness::WellFormed => NSColor::systemGreenColor(),
            WellFormedness::Error { .. } => NSColor::systemRedColor(),
        };
        self.state.setTextColor(Some(&colour));
    }

    /// `Some("Sent … · read-only")` while an older exchange's request is shown in place of
    /// the editor; `None` shows the editor's state again.
    pub fn show_sent(&self, sent: Option<&str>) {
        *self.sent.borrow_mut() = sent.map(str::to_owned);
        match sent {
            Some(text) => {
                self.state.setStringValue(&NSString::from_str(text));
                self.state
                    .setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            None => self.set_state(self.well_formedness.get()),
        }
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
            &[layout::view(summary.clone()), layout::view(hide.clone())],
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
        text_of(&self.ivars().headers)
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
        let key = self.ivars().key;
        let mut drawer = self
            .read(|app| app.project(key).map(|w| w.history_drawer()))
            .flatten()
            .unwrap_or_default();
        if !open && self.drawer_open() {
            drawer.height = Some(self.ivars().history.view().frame().size.height);
        }
        drawer.open = open;
        self.show_drawer(drawer);
        self.command("Could not save the History drawer", |app| {
            app.set_history_drawer(key, drawer)
        });
    }

    /// Shows the drawer as the model remembers it (when the window opens).
    pub fn show_model_drawer(&self) {
        let key = self.ivars().key;
        let drawer = self
            .read(|app| app.project(key).map(|w| w.history_drawer()))
            .flatten()
            .unwrap_or_default();
        self.show_drawer(drawer);
    }

    fn show_drawer(&self, drawer: HistoryDrawer) {
        if let Some(item) = self.table_item() {
            item.setCollapsed(!drawer.open);
        }
        if drawer.open
            && let Some(height) = drawer.height
        {
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
        let open = self.drawer_open();
        let height = self.ivars().history.view().frame().size.height;
        let key = self.ivars().key;
        let mut drawer = self
            .read(|app| app.project(key).map(|w| w.history_drawer()))
            .flatten()
            .unwrap_or_default();
        drawer.open = open;
        if open {
            drawer.height = Some(height);
        }
        self.show_disclosure(open);
        self.command("Could not save the History drawer", |app| {
            app.set_history_drawer(key, drawer)
        });
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
        self.show_spinner(sending);
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

    /// Lists the selected request's history, with the shown exchange selected and ringed
    /// (`HistoryChanged`, `ShownExchangeChanged`).
    pub fn show_history(&self) {
        let key = self.ivars().key;
        let (entries, older) = self
            .read(|app| {
                let window = app.project(key)?;
                let entries = window.history().iter().map(|e| {
                    let server = window.server_label(e.server_id, &e.url);
                    (e.clone(), server)
                });
                let older = window.older_exchange().map(|o| o.entry);
                Some((entries.collect::<Vec<_>>(), older))
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
