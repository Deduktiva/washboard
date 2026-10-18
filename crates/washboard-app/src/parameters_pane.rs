//! A project's Parameters pane in the Settings window (`docs/specs/parameters.md`): one grid
//! with a row per parameter and a column per parameter set, edited in place.
//!
//! The first column is the parameter's name, renamed in place; every other cell is the column's
//! set's value for the row's parameter. A value is stored when its cell's editing ends. An unset
//! cell reads "Not set" and an empty value "Empty", both as placeholders; clearing a cell unsets
//! it, and the context menu's Set Empty Value stores the empty string. Sets are reordered by
//! dragging their column headers, parameters with Move Up and Move Down. Adding a parameter and
//! naming a set go through a sheet with one field, as a header cannot be edited in place.

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSControlTextEditingDelegate, NSLayoutConstraintOrientation,
    NSLayoutPriorityDefaultLow, NSLineBreakMode, NSMenu, NSMenuDelegate, NSMenuItem, NSScrollView,
    NSTableColumn, NSTableView, NSTableViewColumnAutoresizingStyle, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSTextFieldDelegate, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString, ns_string,
};
use washboard_core::model::{ParameterSet, ParameterSetId};
use washboard_ui_model::ProjectKey;

use crate::app::ModelAccess;
use crate::form;
use crate::layout::{self, view};
use crate::settings_window::project_note;

/// The parameter name column's identifier; the others are their sets' ids.
const NAME_COLUMN: &str = "#parameter";
const NAME_COLUMN_WIDTH: f64 = 150.0;
const VALUE_COLUMN_WIDTH: f64 = 120.0;
/// The grid never gets shorter than this; a taller window gives it the rest of the height.
const GRID_MIN_HEIGHT: f64 = 120.0;
const SHEET_WIDTH: f64 = 420.0;
pub const UNSET_PLACEHOLDER: &str = "Not set";
pub const EMPTY_PLACEHOLDER: &str = "Empty";

/// What the name sheet does on Save.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NameEdit {
    AddParameter,
    RenameSet(ParameterSetId),
}

#[derive(Debug)]
pub struct ParametersIvars {
    key: ProjectKey,
    /// The model's parameters and sets as last shown: the grid's rows, and its columns after
    /// the first.
    parameters: RefCell<Vec<String>>,
    sets: RefCell<Vec<ParameterSet>>,
    view: OnceCell<Retained<NSView>>,
    table: OnceCell<Retained<NSTableView>>,
    /// The cell the context menu was opened on: row and column, `-1` for none.
    menu_cell: Cell<(NSInteger, NSInteger)>,
    /// The cell whose edit is being stored, so the reload that follows redraws only that cell:
    /// a full reload replaces every cell view, the one a Tab is moving the editor into too.
    edited_cell: Cell<Option<(usize, usize)>>,
    header_menu: OnceCell<Retained<NSMenu>>,
    /// Set while the pane changes the table's columns itself.
    applying: Cell<bool>,
    sheet: OnceCell<Retained<NSWindow>>,
    body: OnceCell<Retained<NSView>>,
    sheet_title: OnceCell<Retained<NSTextField>>,
    name: OnceCell<Retained<NSTextField>>,
    editing: RefCell<Option<NameEdit>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ParametersPane` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ParametersIvars]
    #[derive(Debug)]
    pub struct ParametersPane;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ParametersPane {}

    // SAFETY: `NSTableViewDataSource` has no safety requirements.
    unsafe impl NSTableViewDataSource for ParametersPane {
        // SAFETY: the signature matches `numberOfRowsInTableView:`.
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().parameters.borrow().len() as NSInteger
        }
    }

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for ParametersPane {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    // `controlTextDidEndEditing:` is declared here, not on `NSTextFieldDelegate`.
    unsafe impl NSControlTextEditingDelegate for ParametersPane {
        // SAFETY: the signature matches `controlTextDidEndEditing:`.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn did_end_editing(&self, notification: &NSNotification) {
            let field = notification
                .object()
                .and_then(|f| f.downcast::<NSTextField>().ok());
            let Some(field) = field else { return };
            let table = self.table();
            let (row, column) = (table.rowForView(&field), table.columnForView(&field));
            if let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) {
                self.cell_edited(row, column, &field.stringValue().to_string());
            }
        }
    }

    // SAFETY: `NSTableViewDelegate` has no safety requirements.
    unsafe impl NSTableViewDelegate for ParametersPane {
        // SAFETY: the signature matches `tableView:viewForTableColumn:row:`.
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_cell(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.view_for(table, column, row)
        }

        // SAFETY: the signature matches `tableView:shouldReorderColumn:toColumn:`.
        #[unsafe(method(tableView:shouldReorderColumn:toColumn:))]
        fn should_reorder(&self, _table: &NSTableView, column: NSInteger, to: NSInteger) -> bool {
            // The parameter names stay first; -1 is asked when a drag starts.
            column != 0 && to != 0
        }

        // Sent once the drag ends; `tableViewColumnDidMove:` comes at every step of it, and
        // storing the order there reloaded the table under the drag.
        // SAFETY: the signature matches `tableView:didDragTableColumn:`.
        #[unsafe(method(tableView:didDragTableColumn:))]
        fn did_drag_column(&self, _table: &NSTableView, _column: &NSTableColumn) {
            if !self.ivars().applying.get() {
                self.columns_moved();
            }
        }
    }

    // SAFETY: `NSMenuDelegate` has no safety requirements.
    unsafe impl NSMenuDelegate for ParametersPane {
        // SAFETY: the signature matches `menuNeedsUpdate:`.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            let cell = if self
                .ivars()
                .header_menu
                .get()
                .is_some_and(|h| std::ptr::eq(&**h, menu))
            {
                (-1, self.header_column_under_mouse())
            } else {
                let table = self.table();
                (table.clickedRow(), table.clickedColumn())
            };
            self.fill_menu(menu, cell);
        }
    }

    impl ParametersPane {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(addParameter:))]
        fn add_parameter_action(&self, _sender: Option<&AnyObject>) {
            self.begin_add_parameter();
        }

        #[unsafe(method(addParameterSet:))]
        fn add_set_action(&self, _sender: Option<&AnyObject>) {
            self.add_set();
        }

        #[unsafe(method(saveName:))]
        fn save_name_action(&self, _sender: Option<&AnyObject>) {
            self.save_name();
        }

        #[unsafe(method(cancelName:))]
        fn cancel_name_action(&self, _sender: Option<&AnyObject>) {
            self.close_sheet();
        }

        #[unsafe(method(renameParameterSet:))]
        fn rename_set_action(&self, _sender: Option<&AnyObject>) {
            if let Some(index) = self.menu_set() {
                self.begin_rename_set(index);
            }
        }

        #[unsafe(method(duplicateParameterSet:))]
        fn duplicate_set_action(&self, _sender: Option<&AnyObject>) {
            if let Some(index) = self.menu_set() {
                self.duplicate_set(index);
            }
        }

        #[unsafe(method(deleteParameterSet:))]
        fn delete_set_action(&self, _sender: Option<&AnyObject>) {
            if let Some(index) = self.menu_set() {
                self.delete_set(index);
            }
        }

        #[unsafe(method(renameParameter:))]
        fn rename_parameter_action(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.ivars().menu_cell.get().0) {
                self.begin_rename_parameter(row);
            }
        }

        #[unsafe(method(moveParameterUp:))]
        fn move_up_action(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.ivars().menu_cell.get().0) {
                self.move_parameter(row, -1);
            }
        }

        #[unsafe(method(moveParameterDown:))]
        fn move_down_action(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.ivars().menu_cell.get().0) {
                self.move_parameter(row, 1);
            }
        }

        #[unsafe(method(deleteParameter:))]
        fn delete_parameter_action(&self, _sender: Option<&AnyObject>) {
            let row = usize::try_from(self.ivars().menu_cell.get().0).ok();
            let name = row.and_then(|r| self.ivars().parameters.borrow().get(r).cloned());
            if let Some(name) = name {
                self.delete_parameter(&name);
            }
        }

        #[unsafe(method(unsetParameterValue:))]
        fn unset_action(&self, _sender: Option<&AnyObject>) {
            self.end_editing();
            let (row, column) = self.ivars().menu_cell.get();
            if let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) {
                self.set_value(row, column, None);
            }
        }

        #[unsafe(method(setEmptyParameterValue:))]
        fn set_empty_action(&self, _sender: Option<&AnyObject>) {
            self.end_editing();
            let (row, column) = self.ivars().menu_cell.get();
            if let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) {
                self.set_value(row, column, Some(""));
            }
        }
    }
);

impl ParametersPane {
    pub(crate) fn new(key: ProjectKey, project: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ParametersIvars {
            key,
            parameters: RefCell::new(Vec::new()),
            sets: RefCell::new(Vec::new()),
            view: OnceCell::new(),
            table: OnceCell::new(),
            menu_cell: Cell::new((-1, -1)),
            edited_cell: Cell::new(None),
            header_menu: OnceCell::new(),
            applying: Cell::new(false),
            sheet: OnceCell::new(),
            body: OnceCell::new(),
            sheet_title: OnceCell::new(),
            name: OnceCell::new(),
            editing: RefCell::new(None),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let (table, scroll) = this.grid(mtm);
        let add_parameter = layout::button("Add Parameter…", &this, sel!(addParameter:), mtm);
        let add_set = layout::button("Add Set", &this, sel!(addParameterSet:), mtm);
        let group = form::group(
            vec![
                form::stretching_table_row(&scroll, GRID_MIN_HEIGHT, mtm),
                form::button_row(vec![view(add_parameter), view(add_set)], mtm),
            ],
            mtm,
        );
        let view = form::filling_page(
            vec![
                project_note(project, mtm),
                form::Section::new(mtm)
                    .header(&form::header("Parameters", mtm))
                    .group(&group)
                    .text(
                        "Requests use a parameter as ${name}; the toolbar's parameter set \
                         decides its value when the request is checked and sent. Clear a cell \
                         to unset it; Set Empty Value in its menu stores an empty value. Drag a \
                         set's column to reorder the sets.",
                    )
                    .build(),
            ],
            mtm,
        );
        let (sheet, body, title, name) = this.name_sheet(mtm);

        let ivars = this.ivars();
        let _ = ivars.view.set(view);
        let _ = ivars.table.set(table);
        let _ = ivars.sheet.set(sheet);
        let _ = ivars.body.set(body);
        let _ = ivars.sheet_title.set(title);
        let _ = ivars.name.set(name);
        this.reload();
        this
    }

    /// The pane's view, which the window shows while the pane is selected.
    pub fn view(&self) -> &Retained<NSView> {
        self.ivars().view.get().expect("set in new()")
    }

    pub fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("set in new()")
    }

    /// The name sheet, attached to the Settings window while a parameter is added or a set
    /// named.
    pub fn sheet(&self) -> &NSWindow {
        self.ivars().sheet.get().expect("set in new()")
    }

    /// The sheet's content, laid out by `form`.
    pub fn sheet_body(&self) -> &NSView {
        self.ivars().body.get().expect("set in new()")
    }

    pub fn name_field(&self) -> &NSTextField {
        self.ivars().name.get().expect("set in new()")
    }

    /// Whether the name sheet is open.
    pub fn is_editing_name(&self) -> bool {
        self.ivars().editing.borrow().is_some()
    }

    /// The grid's column titles, in the order shown: "Parameter", then the sets.
    pub fn column_titles(&self) -> Vec<String> {
        self.table()
            .tableColumns()
            .iter()
            .map(|c| c.title().to_string())
            .collect()
    }

    /// The grid's rows as shown: the parameter's name, then its value in each set, `None`
    /// where unset.
    pub fn rows(&self) -> Vec<Vec<Option<String>>> {
        let sets = self.ivars().sets.borrow();
        self.ivars()
            .parameters
            .borrow()
            .iter()
            .map(|p| {
                let mut row = vec![Some(p.clone())];
                row.extend(sets.iter().map(|set| set.values.get(p).cloned()));
                row
            })
            .collect()
    }

    /// Shows the model's parameters and sets.
    pub fn reload(&self) {
        let key = self.ivars().key;
        let (parameters, sets) = self
            .read_project(key, |w| {
                (w.parameters().to_vec(), w.parameter_sets().to_vec())
            })
            .unwrap_or_default();
        // Compared with the table's columns, not the last list: the first reload builds the
        // name column even when there are no sets, and a renamed set retitles its column.
        let shown: Vec<(String, String)> = self
            .table()
            .tableColumns()
            .iter()
            .map(|c| (c.identifier().to_string(), c.title().to_string()))
            .collect();
        let wanted: Vec<(String, String)> =
            std::iter::once((NAME_COLUMN.to_owned(), "Parameter".to_owned()))
                .chain(sets.iter().map(|s| (s.id.to_string(), s.name.clone())))
                .collect();
        let same_rows = self.ivars().parameters.borrow().len() == parameters.len();
        *self.ivars().parameters.borrow_mut() = parameters;
        *self.ivars().sets.borrow_mut() = sets;
        if shown != wanted {
            self.rebuild_columns();
        }
        match self.ivars().edited_cell.get() {
            // Only the edited cell changed; the rows are the same parameters.
            Some((row, column)) if shown == wanted && same_rows => self.reload_cell(row, column),
            _ => self.table().reloadData(),
        }
        // Renamed or deleted while its sheet was open.
        let gone = match &*self.ivars().editing.borrow() {
            Some(NameEdit::RenameSet(id)) => self.set_index(*id).is_none(),
            _ => false,
        };
        if gone {
            self.close_sheet();
        }
    }

    /// The end of editing cell `row`, `column` with `text`: a parameter's new name, or a
    /// value. Clearing a value unsets it; an empty or unset cell left empty stays as it was.
    pub fn cell_edited(&self, row: usize, column: usize, text: &str) {
        let Some(name) = self.ivars().parameters.borrow().get(row).cloned() else {
            return;
        };
        let key = self.ivars().key;
        self.ivars().edited_cell.set(Some((row, column)));
        let stored = if column == 0 {
            (text.trim() == name).then_some(()).or_else(|| {
                self.command("Could not rename the parameter", |app| {
                    app.rename_parameter(key, &name, text)
                })
            })
        } else {
            let set = self.ivars().sets.borrow().get(column - 1).map(|s| s.id);
            set.and_then(|set| {
                self.command("Could not change the value", |app| {
                    app.edit_parameter_value(key, set, &name, text)
                })
            })
            .map(|_| ())
        };
        self.ivars().edited_cell.set(None);
        if stored.is_none() {
            // The cell still shows the refused text.
            self.reload_cell(row, column);
        }
    }

    /// Starts editing the name of the parameter in `row`.
    pub fn begin_rename_parameter(&self, row: usize) {
        self.end_editing();
        let table = self.table();
        // AppKit raises for a cell that does not exist, which aborts the app.
        if let Ok(row) = NSInteger::try_from(row)
            && row < table.numberOfRows()
            && table.numberOfColumns() > 0
        {
            table.editColumn_row_withEvent_select(0, row, None, true);
        }
    }

    /// Add Set: a set called “New Set”, with the sheet open to name it.
    pub fn add_set(&self) {
        self.end_editing();
        let key = self.ivars().key;
        let added = self.command("Could not add a parameter set", |app| {
            app.add_parameter_set(key)
        });
        if let Some(index) = added.and_then(|id| self.set_index(id)) {
            self.begin_rename_set(index);
        }
    }

    /// Duplicate Set: a copy of the set in column `index` with its values, after it; the
    /// sheet opens to name it, like Add Set.
    pub fn duplicate_set(&self, index: usize) {
        self.end_editing();
        let Some(id) = self.ivars().sets.borrow().get(index).map(|s| s.id) else {
            return;
        };
        let key = self.ivars().key;
        let copy = self.command("Could not duplicate the parameter set", |app| {
            app.duplicate_parameter_set(key, id)
        });
        if let Some(index) = copy.and_then(|id| self.set_index(id)) {
            self.begin_rename_set(index);
        }
    }

    /// Opens the name sheet on the set in column `index` (0 for the first set).
    pub fn begin_rename_set(&self, index: usize) {
        self.end_editing();
        let Some(set) = self.ivars().sets.borrow().get(index).cloned() else {
            return;
        };
        let title = format!("Rename “{}”", set.name);
        self.open_sheet(NameEdit::RenameSet(set.id), &title, &set.name);
    }

    /// Moves the parameter in `row` one place up (`-1`) or down (`1`).
    pub fn move_parameter(&self, row: usize, by: isize) {
        self.end_editing();
        let Some(name) = self.ivars().parameters.borrow().get(row).cloned() else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not move the parameter", |app| {
            app.move_parameter(key, &name, by)
        });
    }

    /// Asks before deleting the set in column `index` (0 for the first set) when it has values.
    pub fn delete_set(&self, index: usize) {
        self.end_editing();
        let Some(id) = self.ivars().sets.borrow().get(index).map(|s| s.id) else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not delete the parameter set", |app| {
            app.ask_delete_parameter_set(key, id)
        });
    }

    /// Asks before deleting `name` when a set has a value for it.
    pub fn delete_parameter(&self, name: &str) {
        self.end_editing();
        let key = self.ivars().key;
        self.command("Could not delete the parameter", |app| {
            app.ask_delete_parameter(key, name)
        });
    }

    /// Opens the name sheet to add a parameter.
    pub fn begin_add_parameter(&self) {
        self.end_editing();
        self.open_sheet(NameEdit::AddParameter, "Add Parameter", "");
    }

    /// The name sheet's Save: closes it once the model took the name, else alerts and stays
    /// open on it.
    pub fn save_name(&self) {
        let Some(edit) = self.ivars().editing.borrow().clone() else {
            return;
        };
        let key = self.ivars().key;
        let name = self.name_field().stringValue().to_string();
        let saved = match edit {
            NameEdit::AddParameter => self.command("Could not add the parameter", |app| {
                app.add_parameter(key, &name)
            }),
            NameEdit::RenameSet(id) => self.command("Could not rename the parameter set", |app| {
                app.rename_parameter_set(key, id, &name)
            }),
        };
        if saved.is_some() {
            self.close_sheet();
        }
    }

    /// The context menu's items for cell `row`, `column` (`-1` for none), as titles with
    /// whether each is enabled; separators are empty titles.
    pub fn menu_items(&self, row: NSInteger, column: NSInteger) -> Vec<(String, bool)> {
        let menu = NSMenu::new(self.mtm());
        self.fill_menu(&menu, (row, column));
        menu.itemArray()
            .iter()
            .map(|item| {
                let title = if item.isSeparatorItem() {
                    String::new()
                } else {
                    item.title().to_string()
                };
                (title, item.isEnabled())
            })
            .collect()
    }

    fn set_value(&self, row: usize, column: usize, value: Option<&str>) {
        let name = self.ivars().parameters.borrow().get(row).cloned();
        let set = column
            .checked_sub(1)
            .and_then(|i| self.ivars().sets.borrow().get(i).map(|s| s.id));
        let (Some(set), Some(name)) = (set, name) else {
            return;
        };
        let key = self.ivars().key;
        let stored = self.command("Could not change the value", |app| {
            app.set_parameter_value(key, set, &name, value)
        });
        if stored.is_none() {
            self.table().reloadData();
        }
    }

    /// The model's order for the sets after the user dragged a column.
    fn columns_moved(&self) {
        let ids: Vec<String> = self
            .table()
            .tableColumns()
            .iter()
            .map(|c| c.identifier().to_string())
            .filter(|id| id != NAME_COLUMN)
            .collect();
        let order: Vec<ParameterSetId> = ids
            .iter()
            .filter_map(|id| {
                let sets = self.ivars().sets.borrow();
                sets.iter().find(|s| s.id.to_string() == *id).map(|s| s.id)
            })
            .collect();
        let current: Vec<ParameterSetId> =
            self.ivars().sets.borrow().iter().map(|s| s.id).collect();
        if order == current {
            return;
        }
        let key = self.ivars().key;
        let moved = self.command("Could not move the parameter set", |app| {
            app.set_parameter_set_order(key, &order)
        });
        if moved.is_none() {
            self.rebuild_columns();
            self.table().reloadData();
        }
    }

    /// One column for the parameter names, then one per set, in the model's order.
    fn rebuild_columns(&self) {
        let mtm = self.mtm();
        let table = self.table();
        let was = self.ivars().applying.replace(true);
        // A cell still being edited would lose its column under the field editor. The pane's
        // own actions have stored it already (`end_editing`); this is for changes from elsewhere.
        table.abortEditing();
        // `tableColumns` is the table's own array, which removing changes: copy it first.
        let old: Vec<Retained<NSTableColumn>> = table.tableColumns().to_vec();
        for column in old {
            table.removeTableColumn(&column);
        }
        let mut columns = vec![(
            NAME_COLUMN.to_owned(),
            "Parameter".to_owned(),
            NAME_COLUMN_WIDTH,
        )];
        columns.extend(
            self.ivars()
                .sets
                .borrow()
                .iter()
                .map(|s| (s.id.to_string(), s.name.clone(), VALUE_COLUMN_WIDTH)),
        );
        for (id, title, width) in columns {
            let column = NSTableColumn::initWithIdentifier(
                NSTableColumn::alloc(mtm),
                &NSString::from_str(&id),
            );
            column.setTitle(&NSString::from_str(&title));
            column.setWidth(width);
            column.setMinWidth(60.0);
            table.addTableColumn(&column);
        }
        self.ivars().applying.set(was);
    }

    fn view_for(
        &self,
        table: &NSTableView,
        column: Option<&NSTableColumn>,
        row: NSInteger,
    ) -> Option<Retained<NSView>> {
        let column = table.columnWithIdentifier(&column?.identifier());
        let (row, column) = (usize::try_from(row).ok()?, usize::try_from(column).ok()?);
        Some(self.cell_view(row, column))
    }

    /// An editable text field for cell `row`, `column`.
    fn cell_view(&self, row: usize, column: usize) -> Retained<NSView> {
        let mtm = self.mtm();
        let name = self.ivars().parameters.borrow().get(row).cloned();
        let (text, placeholder) = if column == 0 {
            (name.unwrap_or_default(), None)
        } else {
            let sets = self.ivars().sets.borrow();
            match name.and_then(|n| sets.get(column - 1)?.values.get(&n).cloned()) {
                None => (String::new(), Some(UNSET_PLACEHOLDER)),
                Some(v) if v.is_empty() => (v, Some(EMPTY_PLACEHOLDER)),
                Some(v) => (v, None),
            }
        };
        let field = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
        layout::truncating(&field, NSLineBreakMode::ByTruncatingTail);
        field.setEditable(true);
        field.setPlaceholderString(placeholder.map(NSString::from_str).as_deref());
        // SAFETY: this pane owns the table and so the field, which holds its delegate weakly.
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
        view(layout::cell(&field, Some(&field), mtm))
    }

    /// The table, in a scroll view that scrolls both ways: many sets run wider than the pane.
    fn grid(&self, mtm: MainThreadMarker) -> (Retained<NSTableView>, Retained<NSScrollView>) {
        let table = NSTableView::new(mtm);
        table.setAllowsColumnReordering(true);
        table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::NoColumnAutoresizing);
        table.setUsesAlternatingRowBackgroundColors(true);
        // SAFETY: this pane owns the table through its view, so it outlives the table's weak
        // data source and delegate references.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        // The items depend on the clicked cell, so both menus are filled as they open.
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        // SAFETY: the table retains its menu; the menu's delegate is this pane, which outlives
        // the table as above.
        unsafe { table.setMenu(Some(&menu)) };
        if let Some(header) = table.headerView() {
            let header_menu = NSMenu::new(mtm);
            header_menu.setAutoenablesItems(false);
            header_menu.setDelegate(Some(ProtocolObject::from_ref(self)));
            // SAFETY: as for the table's menu.
            unsafe { header.setMenu(Some(&header_menu)) };
            let _ = self.ivars().header_menu.set(header_menu);
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(true);
        scroll.setAutohidesScrollers(true);
        // A table reports its columns' total width as its size. At the default priorities that
        // size beat the window's: opening the pane shrank the window to the grid, and adding
        // sets would have widened it. The scroll view takes whatever width the pane has.
        for view in [view(table.clone()), view(scroll.clone())] {
            view.setContentHuggingPriority_forOrientation(
                NSLayoutPriorityDefaultLow,
                NSLayoutConstraintOrientation::Horizontal,
            );
            view.setContentCompressionResistancePriority_forOrientation(
                NSLayoutPriorityDefaultLow,
                NSLayoutConstraintOrientation::Horizontal,
            );
        }
        (table, scroll)
    }

    /// The column under the mouse in the table's header, for the header's menu.
    fn header_column_under_mouse(&self) -> NSInteger {
        let event = NSApplication::sharedApplication(self.mtm()).currentEvent();
        match (event, self.table().headerView()) {
            (Some(event), Some(header)) => {
                header.columnAtPoint(header.convertPoint_fromView(event.locationInWindow(), None))
            }
            _ => -1,
        }
    }

    /// Parameter items for a row, set items for a column, value items for a value cell.
    fn fill_menu(&self, menu: &NSMenu, (row, column): (NSInteger, NSInteger)) {
        self.ivars().menu_cell.set((row, column));
        menu.removeAllItems();
        let parameters = self.ivars().parameters.borrow().clone();
        let mut groups: Vec<Vec<(String, Sel, bool)>> = Vec::new();
        let parameter = usize::try_from(row)
            .ok()
            .and_then(|r| Some((r, parameters.get(r)?.clone())));
        if let Some((r, name)) = &parameter {
            groups.push(vec![
                (
                    format!("Rename Parameter “{name}”"),
                    sel!(renameParameter:),
                    true,
                ),
                ("Move Up".into(), sel!(moveParameterUp:), *r > 0),
                (
                    "Move Down".into(),
                    sel!(moveParameterDown:),
                    r + 1 < parameters.len(),
                ),
                (
                    format!("Delete Parameter “{name}”…"),
                    sel!(deleteParameter:),
                    true,
                ),
            ]);
        }
        let set = self
            .menu_set()
            .and_then(|i| self.ivars().sets.borrow().get(i).cloned());
        if let Some(set) = &set {
            groups.push(vec![
                (
                    format!("Rename Set “{}”…", set.name),
                    sel!(renameParameterSet:),
                    true,
                ),
                (
                    format!("Duplicate Set “{}”", set.name),
                    sel!(duplicateParameterSet:),
                    true,
                ),
                (
                    format!("Delete Set “{}”…", set.name),
                    sel!(deleteParameterSet:),
                    true,
                ),
            ]);
        }
        if let (Some((_, name)), Some(set)) = (&parameter, &set) {
            let value = set.values.get(name);
            groups.push(vec![
                (
                    "Unset Value".into(),
                    sel!(unsetParameterValue:),
                    value.is_some(),
                ),
                (
                    "Set Empty Value".into(),
                    sel!(setEmptyParameterValue:),
                    value.map(String::as_str) != Some(""),
                ),
            ]);
        }
        for (i, group) in groups.into_iter().enumerate() {
            if i > 0 {
                menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            }
            for (title, action, enabled) in group {
                // SAFETY: every action above is an action method of this pane, which takes
                // the sender; the pane owns the table and so the menu, outliving the item's
                // weak target reference.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(self.mtm()),
                        &NSString::from_str(&title),
                        Some(action),
                        ns_string!(""),
                    )
                };
                // SAFETY: as above.
                unsafe { item.setTarget(Some(self)) };
                item.setEnabled(enabled);
                menu.addItem(&item);
            }
        }
    }

    /// Stores the cell being edited, if any, before an action changes the grid: rebuilding the
    /// columns or reloading would otherwise throw the typed text away.
    /// A view-based table does not report its cell edits in `editedRow`; the field editor
    /// sits inside the edited text field, so it is found as the first responder in the table.
    fn end_editing(&self) {
        let table = self.table();
        let Some(window) = table.window() else { return };
        let editing = window
            .firstResponder()
            .and_then(|r| r.downcast::<NSView>().ok())
            .is_some_and(|v| v.isDescendantOf(table));
        if editing {
            window.makeFirstResponder(None);
        }
    }

    fn reload_cell(&self, row: usize, column: usize) {
        let rows = NSIndexSet::indexSetWithIndex(row);
        let columns = NSIndexSet::indexSetWithIndex(column);
        self.table()
            .reloadDataForRowIndexes_columnIndexes(&rows, &columns);
    }

    /// The index of the set whose column the context menu was opened on.
    fn menu_set(&self) -> Option<usize> {
        let column = usize::try_from(self.ivars().menu_cell.get().1).ok()?;
        let id = self.table().tableColumns().iter().nth(column)?.identifier();
        let id = id.to_string();
        self.ivars()
            .sets
            .borrow()
            .iter()
            .position(|s| s.id.to_string() == id)
    }

    fn set_index(&self, id: ParameterSetId) -> Option<usize> {
        self.ivars().sets.borrow().iter().position(|s| s.id == id)
    }

    fn open_sheet(&self, edit: NameEdit, title: &str, name: &str) {
        *self.ivars().editing.borrow_mut() = Some(edit);
        if let Some(label) = self.ivars().sheet_title.get() {
            label.setStringValue(&NSString::from_str(title));
        }
        self.name_field().setStringValue(&NSString::from_str(name));
        let sheet = self.sheet();
        form::fit_window(sheet, self.sheet_body());
        sheet.makeFirstResponder(Some(self.name_field()));
        if sheet.sheetParent().is_none()
            && let Some(parent) = self.view().window()
        {
            parent.beginSheet_completionHandler(sheet, None);
        }
    }

    fn close_sheet(&self) {
        *self.ivars().editing.borrow_mut() = None;
        let sheet = self.sheet();
        sheet.makeFirstResponder(None);
        layout::dismiss(sheet);
    }

    fn name_sheet(
        &self,
        mtm: MainThreadMarker,
    ) -> (
        Retained<NSWindow>,
        Retained<NSView>,
        Retained<NSTextField>,
        Retained<NSTextField>,
    ) {
        let name = NSTextField::textFieldWithString(ns_string!(""), mtm);
        name.setPlaceholderString(Some(ns_string!("Name")));
        let group = form::group(vec![form::field_row("Name", &name, mtm)], mtm);
        let title = form::header("", mtm);
        let cancel = layout::cancel_button("Cancel", self, sel!(cancelName:), mtm);
        let save = layout::default_button("Save", self, sel!(saveName:), mtm);
        let body = form::body(
            vec![
                form::Section::new(mtm)
                    .header(&title)
                    .group(&group)
                    .text(
                        "A parameter's name has letters, digits and _, and does not start with \
                         a digit. Renaming a parameter does not change the requests that use it.",
                    )
                    .build(),
            ],
            form::dialog_buttons(None, vec![view(cancel), view(save)], mtm),
            SHEET_WIDTH,
            mtm,
        );
        let sheet = layout::owned_window(
            ns_string!("Parameter"),
            NSSize::new(SHEET_WIDTH, 200.0),
            NSWindowStyleMask::Titled,
            mtm,
        );
        if let Some(content) = sheet.contentView() {
            form::fill(&content, &body);
        }
        (sheet, body, title, name)
    }
}

impl ModelAccess for ParametersPane {}
