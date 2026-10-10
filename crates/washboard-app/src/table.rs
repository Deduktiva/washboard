//! A text-only table: rows of strings, one label per cell, and a callback for the row the user
//! picks, by clicking it or by moving the selection with the keyboard. Shared by the issues
//! bar, the response history, the HTTP log, the import sheet and the Servers pane.

use std::cell::{Cell, OnceCell, RefCell};
use std::fmt;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSControl, NSControlTextEditingDelegate, NSEvent, NSLineBreakMode,
    NSResponder, NSScrollView, NSTableColumn, NSTableColumnResizingOptions, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString};

use crate::layout;

type OnClick = Box<dyn Fn(usize)>;

#[derive(Debug, Default)]
pub struct KeyTableIvars {
    /// Set while the table handles a key press.
    in_key_down: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - NSTableView has no subclassing requirements beyond its designated initializers, which
    //   we inherit.
    // - `KeyTableView` does not implement `Drop`.
    #[unsafe(super(NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = KeyTableIvars]
    #[derive(Debug)]
    pub struct KeyTableView;

    impl KeyTableView {
        // SAFETY: the signature matches `keyDown:`.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            // A selection change during a key press (↑/↓, Home, End, type-select) is the
            // user's; one made by code or by a mouse down is not, and a click is reported by
            // the table's action.
            let was = self.ivars().in_key_down.replace(true);
            // SAFETY: `keyDown:` takes the event.
            let _: () = unsafe { msg_send![super(self), keyDown: event] };
            self.ivars().in_key_down.set(was);
        }
    }
);

impl KeyTableView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(KeyTableIvars::default());
        // SAFETY: `init` is NSTableView's designated initializer for code-built views.
        unsafe { msg_send![super(this), init] }
    }
}

/// Above and below a wrapped cell's text.
const WRAP_PADDING: f64 = 2.0;

pub struct TableIvars {
    columns: usize,
    rows: RefCell<Vec<Vec<String>>>,
    /// Columns fixed by `fix_column_width`, whose text is centred.
    fixed: RefCell<Vec<usize>>,
    /// Columns set by `wrap_column`, whose text wraps and makes its row taller.
    wrapped: RefCell<Vec<usize>>,
    on_click: RefCell<Option<OnClick>>,
    table: OnceCell<Retained<KeyTableView>>,
    scroll: OnceCell<Retained<NSScrollView>>,
}

impl fmt::Debug for TableIvars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableIvars")
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `TextTable` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TableIvars]
    #[derive(Debug)]
    pub struct TextTable;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for TextTable {}

    // SAFETY: `NSTableViewDataSource` has no safety requirements.
    unsafe impl NSTableViewDataSource for TextTable {
        // SAFETY: the signature matches `numberOfRowsInTableView:`.
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }
    }

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for TextTable {}

    // SAFETY: `NSTableViewDelegate` has no safety requirements.
    unsafe impl NSTableViewDelegate for TextTable {
        // SAFETY: the signature matches `tableView:viewForTableColumn:row:`.
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_cell(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let index = column.map_or(0, |c| {
                usize::try_from(table.columnWithIdentifier(&c.identifier())).unwrap_or(0)
            });
            let rows = self.ivars().rows.borrow();
            let text = usize::try_from(row).ok().and_then(|r| rows.get(r)?.get(index));
            text.map(|text| {
                let mtm = self.mtm();
                let label = match column.filter(|_| self.wraps(index)) {
                    Some(column) => self.wrapping_label(text, column.width()),
                    None => {
                        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
                        layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
                        label
                    }
                };
                if self.ivars().fixed.borrow().contains(&index) {
                    label.setAlignment(NSTextAlignment::Center);
                }
                let cell = layout::cell(&label, Some(&label), mtm);
                Retained::into_super(cell)
            })
        }

        // SAFETY: the signature matches `tableViewSelectionDidChange:`.
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            let table = self.ivars().table.get();
            let by_key = table.is_some_and(|t| t.ivars().in_key_down.get());
            let row = table.map_or(-1, |t| t.selectedRow());
            if let (true, Ok(row)) = (by_key, usize::try_from(row)) {
                self.click(row);
            }
        }

        // SAFETY: the signature matches `tableView:heightOfRow:`.
        #[unsafe(method(tableView:heightOfRow:))]
        fn height_of_row(&self, table: &NSTableView, row: NSInteger) -> f64 {
            let base = table.rowHeight();
            let rows = self.ivars().rows.borrow();
            let Some(cells) = usize::try_from(row).ok().and_then(|r| rows.get(r)) else {
                return base;
            };
            let columns = table.tableColumns();
            self.ivars()
                .wrapped
                .borrow()
                .iter()
                .filter_map(|&i| {
                    let text = cells.get(i)?;
                    let column = columns.iter().nth(i)?;
                    let label = self.wrapping_label(text, column.width());
                    Some(label.intrinsicContentSize().height + 2.0 * WRAP_PADDING)
                })
                .fold(base, f64::max)
        }

        // SAFETY: the signature matches `tableViewColumnDidResize:`.
        #[unsafe(method(tableViewColumnDidResize:))]
        fn column_did_resize(&self, _notification: &NSNotification) {
            // A wider or narrower column wraps its text onto other lines.
            if !self.ivars().wrapped.borrow().is_empty() {
                self.table().reloadData();
            }
        }
    }

    impl TextTable {
        // SAFETY: action methods take the sender and return nothing.
        #[unsafe(method(rowClicked:))]
        fn row_clicked(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.table().clickedRow()) {
                self.click(row);
            }
        }
    }
);

impl TextTable {
    /// A table with one column per title. `titles` empty hides the header (one column).
    pub fn new(titles: &[&str], mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TableIvars {
            columns: titles.len().max(1),
            rows: RefCell::new(Vec::new()),
            fixed: RefCell::new(Vec::new()),
            wrapped: RefCell::new(Vec::new()),
            on_click: RefCell::new(None),
            table: OnceCell::new(),
            scroll: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = KeyTableView::new(mtm);
        for i in 0..this.ivars().columns {
            let id = NSString::from_str(&i.to_string());
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &id);
            if let Some(title) = titles.get(i) {
                column.setTitle(&NSString::from_str(title));
            }
            table.addTableColumn(&column);
        }
        if titles.is_empty() {
            table.setHeaderView(None);
        }
        // SAFETY: whoever owns this object owns the table's view hierarchy too, so it outlives
        // the table's weak data source, delegate and target references.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*this)));
            table.setTarget(Some(&this));
            table.setAction(Some(sel!(rowClicked:)));
        }

        let scroll = layout::vertical_scroll(&table, mtm);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let _ = this.ivars().table.set(table);
        let _ = this.ivars().scroll.set(scroll);
        this
    }

    pub fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("set in new()")
    }

    /// The scroll view to put into a window.
    pub fn view(&self) -> &NSScrollView {
        self.ivars().scroll.get().expect("set in new()")
    }

    /// Fixes `column` at `width` and centres its text, e.g. a narrow ✓/✗ column; the others
    /// share the rest.
    pub fn fix_column_width(&self, column: usize, width: f64) {
        self.ivars().fixed.borrow_mut().push(column);
        if let Some(c) = self.table().tableColumns().iter().nth(column) {
            c.setMinWidth(width);
            c.setMaxWidth(width);
            c.setWidth(width);
            c.setResizingMask(NSTableColumnResizingOptions::empty());
        }
    }

    /// Wraps the text of `column` onto as many lines as it needs, making its row taller,
    /// instead of shortening it: for messages that are only useful when read in full.
    pub fn wrap_column(&self, column: usize) {
        self.ivars().wrapped.borrow_mut().push(column);
        self.table().reloadData();
    }

    fn wraps(&self, column: usize) -> bool {
        self.ivars().wrapped.borrow().contains(&column)
    }

    /// A label for a cell of a wrapped column `width` wide; the same label measures its row.
    fn wrapping_label(&self, text: &str, width: f64) -> Retained<NSTextField> {
        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.mtm());
        label.setSelectable(false);
        label.setPreferredMaxLayoutWidth((width - 2.0 * layout::CELL_INSET).max(1.0));
        label
    }

    pub fn set_rows(&self, rows: Vec<Vec<String>>) {
        *self.ivars().rows.borrow_mut() = rows;
        self.table().reloadData();
    }

    pub fn rows(&self) -> Vec<Vec<String>> {
        self.ivars().rows.borrow().clone()
    }

    /// What happens when the user picks a row: clicks it (also when it was already selected),
    /// or moves the selection to it with the keyboard.
    pub fn on_click(&self, f: impl Fn(usize) + 'static) {
        *self.ivars().on_click.borrow_mut() = Some(Box::new(f));
    }

    /// What picking `row` does; also called by tests.
    pub fn click(&self, row: usize) {
        if let Some(f) = &*self.ivars().on_click.borrow() {
            f(row);
        }
    }
}
