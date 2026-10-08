//! A text-only table: rows of strings, one label per cell, and a click callback. Shared by
//! the issues bar, the response history and the HTTP log.

use std::cell::{OnceCell, RefCell};
use std::fmt;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSControlTextEditingDelegate, NSScrollView, NSTableColumn,
    NSTableColumnResizingOptions, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTextField, NSView,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSString};

type OnClick = Box<dyn Fn(usize)>;

pub struct TableIvars {
    columns: usize,
    rows: RefCell<Vec<Vec<String>>>,
    on_click: RefCell<Option<OnClick>>,
    table: OnceCell<Retained<NSTableView>>,
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
                let label = NSTextField::labelWithString(&NSString::from_str(text), self.mtm());
                Retained::into_super(Retained::into_super(label))
            })
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
            on_click: RefCell::new(None),
            table: OnceCell::new(),
            scroll: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = NSTableView::new(mtm);
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

        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        // Shown only when the content does not fit, also with legacy (always-on) scrollers.
        scroll.setAutohidesScrollers(true);
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

    /// Fixes `column` at `width`, e.g. a narrow ✓/✗ column; the others share the rest.
    pub fn fix_column_width(&self, column: usize, width: f64) {
        if let Some(c) = self.table().tableColumns().iter().nth(column) {
            c.setMinWidth(width);
            c.setMaxWidth(width);
            c.setWidth(width);
            c.setResizingMask(NSTableColumnResizingOptions::empty());
        }
    }

    pub fn set_rows(&self, rows: Vec<Vec<String>>) {
        *self.ivars().rows.borrow_mut() = rows;
        self.table().reloadData();
    }

    pub fn rows(&self) -> Vec<Vec<String>> {
        self.ivars().rows.borrow().clone()
    }

    pub fn on_click(&self, f: impl Fn(usize) + 'static) {
        *self.ivars().on_click.borrow_mut() = Some(Box::new(f));
    }

    /// What a click on `row` does; also called by tests.
    pub fn click(&self, row: usize) {
        if let Some(f) = &*self.ivars().on_click.borrow() {
            f(row);
        }
    }
}
