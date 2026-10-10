//! Project ▸ New Request… (PLAN §4 "Requests"): a sheet with a search field over the WSDL's
//! operations, as `washboard_ui_model::pick_operations` filters and groups them. The keys stay
//! in the search field: ↑/↓ move the highlight, Return creates, Esc cancels, as in Xcode's Open
//! Quickly.

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBorderType, NSButton, NSColor, NSControl, NSControlTextEditingDelegate, NSFont,
    NSLineBreakMode, NSSearchField, NSSearchFieldDelegate, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle, NSTextField, NSTextFieldDelegate,
    NSTextView, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString, ns_string,
};
use washboard_core::model::OperationRef;
use washboard_ui_model::{OperationNode, ProjectKey, ServiceNode, pick_operations};

use crate::app::with_delegate;
use crate::form::{self, PAGE_MARGIN};
use crate::layout::{self, view};

/// The sheet's size until the user resizes it; AppKit then keeps theirs in the user defaults.
const SIZE: NSSize = NSSize::new(480.0, 400.0);
const AUTOSAVE_NAME: &str = "OperationPicker";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Header(String),
    Operation(OperationNode),
}

#[derive(Debug)]
struct PickerViews {
    window: Retained<NSWindow>,
    search: Retained<NSSearchField>,
    table: Retained<NSTableView>,
    empty: Retained<NSTextField>,
    create: Retained<NSButton>,
}

#[derive(Debug)]
pub struct PickerIvars {
    key: ProjectKey,
    services: Vec<ServiceNode>,
    rows: RefCell<Vec<Row>>,
    /// Create or Cancel ended the sheet; a second key press must not create again.
    done: Cell<bool>,
    views: OnceCell<PickerViews>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `OperationPicker` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PickerIvars]
    #[derive(Debug)]
    pub struct OperationPicker;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for OperationPicker {}

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for OperationPicker {}

    // SAFETY: `NSSearchFieldDelegate` has no safety requirements.
    unsafe impl NSSearchFieldDelegate for OperationPicker {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for OperationPicker {
        // SAFETY: the signature matches `controlTextDidChange:`.
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.filter();
        }

        // SAFETY: the signature matches `control:textView:doCommandBySelector:`.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text_view: &NSTextView, command: Sel) -> bool {
            self.key_command(command)
        }
    }

    // SAFETY: `NSTableViewDataSource` has no safety requirements.
    unsafe impl NSTableViewDataSource for OperationPicker {
        // SAFETY: the signature matches `numberOfRowsInTableView:`.
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }
    }

    // SAFETY: `NSTableViewDelegate` has no safety requirements.
    unsafe impl NSTableViewDelegate for OperationPicker {
        // SAFETY: the signature matches `tableView:viewForTableColumn:row:`.
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_cell(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.row(row).map(|row| row_view(&row, self.mtm()))
        }

        // SAFETY: the signature matches `tableView:isGroupRow:`.
        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.row(row), Some(Row::Header(_)))
        }

        // SAFETY: the signature matches `tableView:shouldSelectRow:`.
        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.row(row), Some(Row::Operation(_)))
        }

        // SAFETY: the signature matches `tableViewSelectionDidChange:`.
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            self.highlight_changed();
        }
    }

    impl OperationPicker {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(create:))]
        fn create_action(&self, _sender: Option<&AnyObject>) {
            self.create();
        }

        #[unsafe(method(cancel:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) {
            self.cancel();
        }

        #[unsafe(method(rowDoubleClicked:))]
        fn double_clicked(&self, _sender: Option<&AnyObject>) {
            if self.table().clickedRow() >= 0 {
                self.create();
            }
        }
    }
);

impl OperationPicker {
    /// A picker over the project's operations, `preferred` highlighted when it is one of them.
    /// Shown with [`present`](Self::present).
    pub fn new(
        key: ProjectKey,
        services: Vec<ServiceNode>,
        preferred: Option<OperationRef>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PickerIvars {
            key,
            services,
            rows: RefCell::new(Vec::new()),
            done: Cell::new(false),
            views: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(ns_string!("Operation, element, service or port")));
        let table = NSTableView::new(mtm);
        table.addTableColumn(&NSTableColumn::initWithIdentifier(
            NSTableColumn::alloc(mtm),
            ns_string!("operation"),
        ));
        table.setHeaderView(None);
        table.setStyle(NSTableViewStyle::Inset);
        // SAFETY: this object owns the sheet and so the field and table, which hold it
        // weakly as their delegate, data source and target.
        unsafe {
            search.setDelegate(Some(ProtocolObject::from_ref(&*this)));
            table.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*this)));
            table.setTarget(Some(&this));
            table.setDoubleAction(Some(sel!(rowDoubleClicked:)));
        }
        // The keys stay in the search field.
        table.setRefusesFirstResponder(true);
        let scroll = layout::vertical_scroll(&table, mtm);
        scroll.setBorderType(NSBorderType::BezelBorder);
        let empty = NSTextField::labelWithString(ns_string!("No operation matches"), mtm);
        empty.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let button = |title: &str, action: Sel, key: &NSString| {
            let target: &AnyObject = &this;
            // SAFETY: this object owns the sheet and so the button, whose target is weak.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(target),
                    Some(action),
                    mtm,
                )
            };
            button.setKeyEquivalent(key);
            button
        };
        let cancel = button("Cancel", sel!(cancel:), ns_string!("\u{1b}"));
        let create = button("Create", sel!(create:), ns_string!("\r"));
        let buttons = form::dialog_buttons(None, vec![view(cancel), view(create.clone())], mtm);

        let content = NSView::new(mtm);
        let column = [view(search.clone()), view(scroll.clone()), buttons];
        for (i, v) in column.iter().enumerate() {
            v.setTranslatesAutoresizingMaskIntoConstraints(false);
            content.addSubview(v);
            let top = match i {
                0 => v
                    .topAnchor()
                    .constraintEqualToAnchor_constant(&content.topAnchor(), PAGE_MARGIN),
                _ => v
                    .topAnchor()
                    .constraintEqualToAnchor_constant(&column[i - 1].bottomAnchor(), 12.0),
            };
            for constraint in [
                top,
                v.leadingAnchor()
                    .constraintEqualToAnchor_constant(&content.leadingAnchor(), PAGE_MARGIN),
                v.trailingAnchor()
                    .constraintEqualToAnchor_constant(&content.trailingAnchor(), -PAGE_MARGIN),
            ] {
                constraint.setActive(true);
            }
        }
        column[2]
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&content.bottomAnchor(), -PAGE_MARGIN)
            .setActive(true);
        empty.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&empty);
        empty
            .centerXAnchor()
            .constraintEqualToAnchor(&scroll.centerXAnchor())
            .setActive(true);
        empty
            .centerYAnchor()
            .constraintEqualToAnchor(&scroll.centerYAnchor())
            .setActive(true);

        let window = layout::owned_window(
            ns_string!("New Request"),
            SIZE,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            mtm,
        );
        window.setContentView(Some(&content));
        window.setContentMinSize(NSSize::new(360.0, 240.0));
        window.setFrameAutosaveName(&NSString::from_str(AUTOSAVE_NAME));
        window.setInitialFirstResponder(Some(&search));
        let _ = this.ivars().views.set(PickerViews {
            window,
            search,
            table,
            empty,
            create,
        });
        this.filter();
        if let Some(row) = preferred.and_then(|op| this.row_of(|o| o.operation == op)) {
            this.highlight(row);
        }
        this
    }

    pub fn window(&self) -> &NSWindow {
        &self.views().window
    }

    pub fn create_button(&self) -> &NSButton {
        &self.views().create
    }

    /// Attaches the picker to the project window.
    pub fn present(&self, parent: &NSWindow) {
        parent.beginSheet_completionHandler(self.window(), None);
    }

    /// The highlighted operation, also an unsupported one.
    pub fn highlighted(&self) -> Option<OperationRef> {
        match self.row(self.table().selectedRow())? {
            Row::Operation(op) => Some(op.operation),
            Row::Header(_) => None,
        }
    }

    /// The list as shown: headers as they read, operations by name.
    pub fn shown(&self) -> Vec<String> {
        let rows = self.ivars().rows.borrow();
        rows.iter()
            .map(|row| match row {
                Row::Header(title) => title.clone(),
                Row::Operation(op) => op.name().to_string(),
            })
            .collect()
    }

    /// Types `query` into the search field.
    pub fn search(&self, query: &str) {
        self.views()
            .search
            .setStringValue(&NSString::from_str(query));
        self.filter();
    }

    /// The search field's key commands: ↑/↓ move the highlight, Return creates and Esc
    /// cancels. Whether it handled `command`; also called by tests.
    pub fn key_command(&self, command: Sel) -> bool {
        if command == sel!(moveUp:) || command == sel!(moveDown:) {
            let down = command == sel!(moveDown:);
            let rows = self.ivars().rows.borrow().len();
            let from = usize::try_from(self.table().selectedRow()).ok();
            let next = (0..rows)
                .filter(|&r| matches!(self.row(r as NSInteger), Some(Row::Operation(_))))
                .filter(|&r| from.is_none_or(|f| if down { r > f } else { r < f }));
            let next = if down { next.min() } else { next.max() };
            if let Some(row) = next {
                self.highlight(row);
            }
        } else if command == sel!(insertNewline:) {
            self.create();
        } else if command == sel!(cancelOperation:) {
            self.cancel();
        } else {
            return false;
        }
        true
    }

    /// Creates a request for the highlighted operation and ends the sheet; does nothing while
    /// Create is disabled.
    pub fn create(&self) {
        let Some(operation) = self.highlighted() else {
            return;
        };
        if self.ivars().done.get() || !self.create_button().isEnabled() {
            return;
        }
        self.end();
        let key = self.ivars().key;
        with_delegate(self.mtm(), |d| {
            if let Some(project) = d.project(key) {
                project.sidebar().new_request(operation);
            }
        });
    }

    pub fn cancel(&self) {
        self.end();
    }

    fn end(&self) {
        self.ivars().done.set(true);
        if let Some(parent) = self.window().sheetParent() {
            parent.endSheet(self.window());
        }
    }

    fn views(&self) -> &PickerViews {
        self.ivars().views.get().expect("set in new()")
    }

    fn table(&self) -> &NSTableView {
        &self.views().table
    }

    fn row(&self, row: NSInteger) -> Option<Row> {
        let index = usize::try_from(row).ok()?;
        self.ivars().rows.borrow().get(index).cloned()
    }

    fn row_of(&self, f: impl Fn(&OperationNode) -> bool) -> Option<usize> {
        let rows = self.ivars().rows.borrow();
        rows.iter()
            .position(|r| matches!(r, Row::Operation(op) if f(op)))
    }

    /// Shows the operations matching the search field, the first one that can be created
    /// highlighted (else the first one).
    fn filter(&self) {
        let query = self.views().search.stringValue().to_string();
        let rows: Vec<Row> = pick_operations(&self.ivars().services, &query)
            .into_iter()
            .flat_map(|section| {
                let header = section.title.map(Row::Header);
                header
                    .into_iter()
                    .chain(section.rows.into_iter().map(Row::Operation))
            })
            .collect();
        self.views().empty.setHidden(!rows.is_empty());
        *self.ivars().rows.borrow_mut() = rows;
        self.table().reloadData();
        let first = self
            .row_of(|op| op.unsupported.is_none())
            .or_else(|| self.row_of(|_| true));
        match first {
            Some(row) => self.highlight(row),
            None => {
                self.table()
                    .selectRowIndexes_byExtendingSelection(&NSIndexSet::new(), false);
                self.highlight_changed();
            }
        }
    }

    fn highlight(&self, row: usize) {
        self.table()
            .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        self.table().scrollRowToVisible(row as NSInteger);
        self.highlight_changed();
    }

    /// Create works on an operation that can be created.
    fn highlight_changed(&self) {
        let selected = self.row(self.table().selectedRow());
        let can = matches!(selected, Some(Row::Operation(op)) if op.unsupported.is_none());
        self.create_button().setEnabled(can);
    }
}

/// A header, or an operation's name with its input element after it in secondary text;
/// greyed with its reason as the tool tip when it cannot be created.
fn row_view(row: &Row, mtm: MainThreadMarker) -> Retained<NSView> {
    let (name, input, unsupported) = match row {
        Row::Header(title) => (title.as_str(), "", None),
        Row::Operation(op) => (op.name(), op.input.as_str(), op.unsupported.as_deref()),
    };
    let label = NSTextField::labelWithString(&NSString::from_str(name), mtm);
    layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
    let detail = layout::small_label(input, mtm);
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    layout::truncating(&detail, NSLineBreakMode::ByTruncatingMiddle);
    if let Row::Header(_) = row {
        label.setFont(Some(&NSFont::boldSystemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    let cell = layout::cell(
        &layout::row(&[view(label.clone()), view(detail)], mtm),
        Some(&label),
        mtm,
    );
    if let Some(why) = unsupported {
        label.setTextColor(Some(&NSColor::disabledControlTextColor()));
        cell.setToolTip(Some(&NSString::from_str(why)));
    }
    Retained::into_super(cell)
}
