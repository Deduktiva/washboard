//! The project window's source list: REQUESTS, the requests' folder tree (PLAN §8), with
//! unsaved (•) and invalid (⚠) markers on requests, inline rename of requests and folders,
//! drag and drop moves, and a context menu per row. Creating a request is the operation
//! picker's job; the WSDL's operations are listed there, unsupported ones included.
//!
//! The rows are the model's [`Sidebar`]; this controller only draws them and turns selection,
//! rename, drops and disclosure into model commands. `NSOutlineView` identifies rows by object
//! pointer and does not retain its items, so every node is an Objective-C object
//! (`SidebarNode`) owned by the tree in `SidebarController`, rebuilt on `SidebarChanged`.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSColor, NSControl, NSControlTextEditingDelegate, NSDragOperation,
    NSDraggingInfo, NSDraggingSession, NSEvent, NSImage, NSImageView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh, NSLineBreakMode, NSMenu,
    NSMenuDelegate, NSMenuItem, NSOutlineView, NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSOutlineViewDropOnItemIndex, NSPasteboardItem, NSPasteboardWriting, NSResponder, NSStackView,
    NSTableColumn, NSTableView, NSTableViewStyle, NSTextField, NSTextFieldDelegate, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSString,
    ns_string,
};
use washboard_core::model::{OperationRef, RequestId};
use washboard_ui_model::{FolderNode, ProjectKey, SchemaState, Sidebar};

use crate::app::{ModelAccess, with_delegate};
use crate::colors;
use crate::layout;
use crate::menu::{self, BACKSPACE, CMD, Entry, NONE, OPT_CMD, item};

/// The pasteboard type of a row being dragged inside the sidebar. Only this outline view
/// accepts it; the row itself is kept in the controller while the drag lasts.
fn drag_type() -> &'static NSString {
    ns_string!("at.deduktiva.washboard.sidebar-row")
}

/// What a sidebar row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// The REQUESTS header, which stands for the top level of `requests/`.
    Group,
    Folder,
    Request,
}

#[derive(Debug)]
enum NodeData {
    Group,
    Folder {
        path: String,
    },
    Request {
        id: RequestId,
        folder: String,
        dirty: bool,
        invalid: bool,
    },
}

#[derive(Debug)]
pub struct NodeIvars {
    data: NodeData,
    title: String,
    children: Vec<Retained<SidebarNode>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SidebarNode` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NodeIvars]
    #[derive(Debug)]
    pub struct SidebarNode;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SidebarNode {}
);

impl SidebarNode {
    fn new(
        data: NodeData,
        title: &str,
        children: Vec<Retained<SidebarNode>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NodeIvars {
            data,
            title: title.to_owned(),
            children,
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn kind(&self) -> NodeKind {
        match self.ivars().data {
            NodeData::Group => NodeKind::Group,
            NodeData::Folder { .. } => NodeKind::Folder,
            NodeData::Request { .. } => NodeKind::Request,
        }
    }

    pub fn title(&self) -> String {
        self.ivars().title.clone()
    }

    pub fn children(&self) -> &[Retained<SidebarNode>] {
        &self.ivars().children
    }

    /// The request a request row stands for.
    pub fn request(&self) -> Option<RequestId> {
        match self.ivars().data {
            NodeData::Request { id, .. } => Some(id),
            _ => None,
        }
    }

    /// A folder row's path; `""` for the REQUESTS header, the top level.
    pub fn folder(&self) -> Option<&str> {
        match &self.ivars().data {
            NodeData::Group => Some(""),
            NodeData::Folder { path } => Some(path),
            NodeData::Request { .. } => None,
        }
    }

    /// The folder a row is in, or is: a request's folder, a folder's own path.
    fn containing_folder(&self) -> &str {
        match &self.ivars().data {
            NodeData::Group => "",
            NodeData::Folder { path } => path,
            NodeData::Request { folder, .. } => folder,
        }
    }

    /// The (unsaved, invalid) markers of a request row.
    pub fn markers(&self) -> (bool, bool) {
        match self.ivars().data {
            NodeData::Request { dirty, invalid, .. } => (dirty, invalid),
            _ => (false, false),
        }
    }
}

/// The REQUESTS group for `sidebar`: its folder tree, folders first.
fn tree(sidebar: &Sidebar, mtm: MainThreadMarker) -> Vec<Retained<SidebarNode>> {
    fn children(
        folder: &FolderNode,
        sidebar: &Sidebar,
        mtm: MainThreadMarker,
    ) -> Vec<Retained<SidebarNode>> {
        let folders = folder.folders.iter().map(|f| {
            let data = NodeData::Folder {
                path: f.path.clone(),
            };
            SidebarNode::new(data, &f.name, children(f, sidebar, mtm), mtm)
        });
        let requests = folder.requests.iter().filter_map(|id| {
            let r = sidebar.request(*id)?;
            let data = NodeData::Request {
                id: r.id,
                folder: r.folder.clone(),
                dirty: r.dirty,
                invalid: r.invalid,
            };
            Some(SidebarNode::new(data, &r.name, vec![], mtm))
        });
        folders.chain(requests).collect()
    }
    let requests = children(&sidebar.tree, sidebar, mtm);
    vec![SidebarNode::new(NodeData::Group, "REQUESTS", requests, mtm)]
}

/// The node behind an outline item. Every item the outline view hands back is one of ours.
fn node(item: &AnyObject) -> &SidebarNode {
    item.downcast_ref::<SidebarNode>()
        .expect("sidebar items are SidebarNodes")
}

/// The node a context menu item was made for.
fn menu_node(sender: Option<&AnyObject>) -> Option<Retained<SidebarNode>> {
    let item = sender?.downcast_ref::<NSMenuItem>()?;
    item.representedObject()?.downcast::<SidebarNode>().ok()
}

define_class!(
    // SAFETY:
    // - NSOutlineView has no subclassing requirements beyond its designated initializers,
    //   which we inherit.
    // - `SidebarOutlineView` does not implement `Drop`.
    #[unsafe(super(NSOutlineView, NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    pub struct SidebarOutlineView;

    impl SidebarOutlineView {
        // SAFETY: the signature matches `keyDown:`.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let is_return = event.charactersIgnoringModifiers().is_some_and(|c| {
                c.to_string() == "\r"
            });
            if is_return && self.selectedRow() >= 0 {
                // Return renames the selected request or folder, as in Finder.
                // SAFETY: `renameRequest:` takes the sender.
                let _: () = unsafe { msg_send![self, renameRequest: None::<&AnyObject>] };
            } else {
                // SAFETY: `keyDown:` takes the event.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        // SAFETY: the signature matches the menu action.
        #[unsafe(method(renameRequest:))]
        fn rename_request(&self, _sender: Option<&AnyObject>) {
            self.edit_row(self.selectedRow());
        }
    }
);

impl SidebarOutlineView {
    /// Starts inline rename of `row` if it is a request or a folder.
    fn edit_row(&self, row: NSInteger) {
        let Some(item) = self.itemAtRow(row) else {
            return;
        };
        if matches!(node(&item).kind(), NodeKind::Request | NodeKind::Folder) {
            self.editColumn_row_withEvent_select(0, row, None, true);
        }
    }
}

#[derive(Debug)]
pub struct SidebarIvars {
    key: ProjectKey,
    roots: RefCell<Vec<Retained<SidebarNode>>>,
    outline: OnceCell<Retained<SidebarOutlineView>>,
    /// Set while the controller changes the outline itself, so its own selection and
    /// disclosure changes don't go back to the model as commands.
    applying: Cell<bool>,
    /// A reload that arrived while a name was being edited; it would have ended the edit.
    stale: Cell<bool>,
    /// The row being dragged, from the drag's start to its end.
    dragged: RefCell<Option<Retained<SidebarNode>>>,
}

/// A request's context menu. The File menu keeps the same commands for the selected
/// request, with the same shortcuts.
const REQUEST_MENU: &[Entry] = &[
    item("New Request…", c"sidebarNewRequest:", "n", CMD),
    item("New Folder", c"sidebarNewFolder:", "n", OPT_CMD),
    Entry::Separator,
    item("Rename", c"sidebarRename:", "", NONE),
    item("Duplicate", c"sidebarDuplicateRequest:", "d", CMD),
    item("Validate", c"sidebarValidateRequest:", "b", CMD),
    item("Delete", c"sidebarDelete:", BACKSPACE, CMD),
];

/// A folder's context menu: New Request… and New Folder create inside it.
const FOLDER_MENU: &[Entry] = &[
    item("New Request…", c"sidebarNewRequest:", "n", CMD),
    item("New Folder", c"sidebarNewFolder:", "n", OPT_CMD),
    Entry::Separator,
    item("Rename", c"sidebarRename:", "", NONE),
    item("Delete", c"sidebarDelete:", BACKSPACE, CMD),
];

/// The REQUESTS header's context menu: both create at the top level.
const HEADER_MENU: &[Entry] = &[
    item("New Request…", c"sidebarNewRequest:", "n", CMD),
    item("New Folder", c"sidebarNewFolder:", "n", OPT_CMD),
];

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SidebarController` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SidebarIvars]
    #[derive(Debug)]
    pub struct SidebarController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SidebarController {}

    // SAFETY: `NSOutlineViewDataSource` has no safety requirements.
    unsafe impl NSOutlineViewDataSource for SidebarController {
        // SAFETY: the signature matches `outlineView:numberOfChildrenOfItem:`.
        #[unsafe(method(outlineView:numberOfChildrenOfItem:))]
        fn number_of_children(
            &self,
            _outline: &NSOutlineView,
            item: Option<&AnyObject>,
        ) -> NSInteger {
            match item {
                None => self.ivars().roots.borrow().len() as NSInteger,
                Some(item) => node(item).children().len() as NSInteger,
            }
        }

        // SAFETY: the signature matches `outlineView:child:ofItem:`; the child is retained by
        // the tree, which outlives the outline view's use of it (a rebuild reloads the
        // outline view before the old tree goes).
        #[unsafe(method_id(outlineView:child:ofItem:))]
        fn child(
            &self,
            _outline: &NSOutlineView,
            index: NSInteger,
            item: Option<&AnyObject>,
        ) -> Retained<AnyObject> {
            let index = usize::try_from(index).expect("AppKit asks for a valid index");
            let child = match item {
                None => self.ivars().roots.borrow()[index].clone(),
                Some(item) => node(item).children()[index].clone(),
            };
            Retained::into_super(Retained::into_super(child))
        }

        // SAFETY: the signature matches `outlineView:isItemExpandable:`.
        #[unsafe(method(outlineView:isItemExpandable:))]
        fn is_expandable(&self, _outline: &NSOutlineView, item: &AnyObject) -> bool {
            !node(item).children().is_empty()
        }

        // SAFETY: the signature matches `outlineView:pasteboardWriterForItem:`.
        #[unsafe(method_id(outlineView:pasteboardWriterForItem:))]
        fn pasteboard_writer(
            &self,
            _outline: &NSOutlineView,
            item: &AnyObject,
        ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            self.drag_started(node(item))
        }

        // SAFETY: the signature matches `outlineView:validateDrop:proposedItem:proposedChildIndex:`.
        #[unsafe(method(outlineView:validateDrop:proposedItem:proposedChildIndex:))]
        fn validate_drop(
            &self,
            outline: &NSOutlineView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            item: Option<&AnyObject>,
            _index: NSInteger,
        ) -> NSDragOperation {
            self.validate_drop_on(outline, info, item)
        }

        // SAFETY: the signature matches `outlineView:acceptDrop:item:childIndex:`.
        #[unsafe(method(outlineView:acceptDrop:item:childIndex:))]
        fn accept_drop(
            &self,
            outline: &NSOutlineView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            item: Option<&AnyObject>,
            _index: NSInteger,
        ) -> bool {
            self.accept_drop_on(outline, info, item)
        }

        // SAFETY: the signature matches `outlineView:draggingSession:endedAtPoint:operation:`.
        #[unsafe(method(outlineView:draggingSession:endedAtPoint:operation:))]
        fn dragging_ended(
            &self,
            _outline: &NSOutlineView,
            _session: &NSDraggingSession,
            _point: NSPoint,
            _operation: NSDragOperation,
        ) {
            self.ivars().dragged.borrow_mut().take();
        }
    }

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for SidebarController {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    // `controlTextDidEndEditing:` is declared here, not on `NSTextFieldDelegate`; objc2 checks
    // that at class registration.
    unsafe impl NSControlTextEditingDelegate for SidebarController {
        // SAFETY: the signature matches `controlTextDidEndEditing:`.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn did_end_editing(&self, notification: &NSNotification) {
            let field = notification
                .object()
                .and_then(|f| f.downcast::<NSTextField>().ok());
            if let Some(field) = field {
                self.rename_ended(&field);
            }
            if self.ivars().stale.replace(false) {
                self.reload_now();
            }
        }
    }

    // SAFETY: `NSOutlineViewDelegate` has no safety requirements.
    unsafe impl NSOutlineViewDelegate for SidebarController {
        // SAFETY: the signature matches `outlineView:viewForTableColumn:item:`.
        #[unsafe(method_id(outlineView:viewForTableColumn:item:))]
        fn view_for_item(
            &self,
            _outline: &NSOutlineView,
            _column: Option<&NSTableColumn>,
            item: &AnyObject,
        ) -> Option<Retained<NSView>> {
            Some(self.row_view(node(item)))
        }

        // SAFETY: the signature matches `outlineView:isGroupItem:`.
        #[unsafe(method(outlineView:isGroupItem:))]
        fn is_group(&self, _outline: &NSOutlineView, item: &AnyObject) -> bool {
            node(item).kind() == NodeKind::Group
        }

        // SAFETY: the signature matches `outlineView:shouldSelectItem:`.
        #[unsafe(method(outlineView:shouldSelectItem:))]
        fn should_select(&self, _outline: &NSOutlineView, item: &AnyObject) -> bool {
            node(item).kind() != NodeKind::Group
        }

        // SAFETY: the signature matches `outlineViewSelectionDidChange:`.
        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            if !self.ivars().applying.get() {
                self.selection_changed_by_user();
            }
        }

        // SAFETY: the signature matches `outlineViewItemDidExpand:`.
        #[unsafe(method(outlineViewItemDidExpand:))]
        fn item_did_expand(&self, notification: &NSNotification) {
            self.disclosure_changed(notification, false);
        }

        // SAFETY: the signature matches `outlineViewItemDidCollapse:`.
        #[unsafe(method(outlineViewItemDidCollapse:))]
        fn item_did_collapse(&self, notification: &NSNotification) {
            self.disclosure_changed(notification, true);
        }
    }

    // SAFETY: `NSMenuDelegate` has no safety requirements.
    unsafe impl NSMenuDelegate for SidebarController {
        // SAFETY: the signature matches `menuNeedsUpdate:`.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            // The row that was right-clicked (or control-clicked), not the selected one.
            let row = self.outline().map_or(-1, |o| o.clickedRow());
            self.fill_context_menu(menu, row);
        }
    }

    // Context menu actions. Each item's represented object is the row's node.
    impl SidebarController {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(sidebarNewRequest:))]
        fn context_new_request(&self, sender: Option<&AnyObject>) {
            // The picker files the request into the selected folder, so a folder's item
            // selects it first, a request's item selects the request's folder, and the
            // header's (or a top-level request's) clears the selection for the default place.
            let Some(node) = menu_node(sender) else { return };
            let folder = match node.kind() {
                NodeKind::Folder => Some(node),
                NodeKind::Request => self.folder_node(node.containing_folder()),
                NodeKind::Group => None,
            };
            match folder {
                Some(folder) => {
                    self.select_node(&folder);
                }
                None => self.select_none(),
            }
            let key = self.ivars().key;
            with_delegate(self.mtm(), |d| d.project(key)?.show_operation_picker());
        }

        #[unsafe(method(sidebarNewFolder:))]
        fn context_new_folder(&self, sender: Option<&AnyObject>) {
            if let Some(node) = menu_node(sender) {
                self.new_folder_in(node.containing_folder());
            }
        }

        #[unsafe(method(sidebarRename:))]
        fn context_rename(&self, sender: Option<&AnyObject>) {
            let Some(node) = menu_node(sender) else { return };
            if self.select_node(&node)
                && let Some(outline) = self.outline()
            {
                outline.edit_row(outline.selectedRow());
            }
        }

        #[unsafe(method(sidebarDuplicateRequest:))]
        fn context_duplicate(&self, sender: Option<&AnyObject>) {
            if let Some(request) = menu_node(sender).and_then(|n| n.request()) {
                self.duplicate_request(request);
            }
        }

        #[unsafe(method(sidebarValidateRequest:))]
        fn context_validate(&self, sender: Option<&AnyObject>) {
            let Some(node) = menu_node(sender) else { return };
            if node.kind() == NodeKind::Request && self.select_node(&node) {
                let key = self.ivars().key;
                self.command("Could not validate the request", |app| app.validate(key));
            }
        }

        #[unsafe(method(sidebarDelete:))]
        fn context_delete(&self, sender: Option<&AnyObject>) {
            let Some(node) = menu_node(sender) else { return };
            match (node.request(), node.folder()) {
                (Some(request), _) => self.delete_request(request),
                (None, Some(path)) if !path.is_empty() => self.delete_folder(path),
                _ => {}
            }
        }
    }

    impl SidebarController {
        // SAFETY: the signature matches an action method; the outline view's double action.
        #[unsafe(method(sidebarDoubleClicked:))]
        fn double_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(outline) = self.outline() else {
                return;
            };
            let row = outline.clickedRow();
            if row < 0 || self.clicked_disclosure(outline, row) {
                // A double-click on a folder's triangle only toggles it.
                return;
            }
            outline.edit_row(row);
        }
    }
);

impl SidebarController {
    pub fn new(key: ProjectKey, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SidebarIvars {
            key,
            roots: RefCell::new(Vec::new()),
            outline: OnceCell::new(),
            applying: Cell::new(false),
            stale: Cell::new(false),
            dragged: RefCell::new(None),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn roots(&self) -> Vec<Retained<SidebarNode>> {
        self.ivars().roots.borrow().clone()
    }

    /// The outline view, once `outline_view` has built it.
    pub fn outline(&self) -> Option<&SidebarOutlineView> {
        self.ivars().outline.get().map(|o| &**o)
    }

    /// Builds the outline view with this controller as data source and delegate.
    pub fn outline_view(&self, mtm: MainThreadMarker) -> Retained<SidebarOutlineView> {
        // SAFETY: `init` is NSOutlineView's designated initializer for code-built views.
        let outline: Retained<SidebarOutlineView> =
            unsafe { msg_send![SidebarOutlineView::alloc(mtm), init] };
        let column =
            NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("name"));
        outline.addTableColumn(&column);
        // SAFETY: `column` was just added to this outline view.
        unsafe { outline.setOutlineTableColumn(Some(&column)) };
        outline.setHeaderView(None);
        outline.setStyle(NSTableViewStyle::SourceList);
        // A floating group row draws a separator under itself. At the top of the list the first
        // header counts as floating, so the line came and went with hover redraws; the sidebar
        // is short enough that headers need not stay pinned while scrolling.
        outline.setFloatsGroupRows(false);
        // Rows move inside the sidebar only, never to or from another app.
        outline.registerForDraggedTypes(&NSArray::from_slice(&[drag_type()]));
        outline.setDraggingSourceOperationMask_forLocal(NSDragOperation::Move, true);
        outline.setDraggingSourceOperationMask_forLocal(NSDragOperation::None, false);
        // SAFETY: the project window controller owns this controller and the outline view's
        // window, so the controller outlives the outline view's weak references to it, the
        // target included; `sidebarDoubleClicked:` takes the sender.
        unsafe {
            outline.setDataSource(Some(ProtocolObject::from_ref(self)));
            outline.setDelegate(Some(ProtocolObject::from_ref(self)));
            outline.setTarget(Some(self));
            outline.setDoubleAction(Some(sel!(sidebarDoubleClicked:)));
        }
        // The items depend on the clicked row, so the menu is filled as it opens.
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        // SAFETY: the outline view retains its menu; the menu's delegate is this controller,
        // which outlives the outline view as above.
        unsafe { outline.setMenu(Some(&menu)) };
        let _ = self.ivars().outline.set(outline.clone());
        outline
    }

    /// The context menu for `row`: New Request… and New Folder, creating in the row's folder,
    /// then Rename, Duplicate, Validate and Delete on a request, or Rename and Delete on a
    /// folder; only the two on the REQUESTS header; nothing elsewhere, so no menu opens.
    pub fn fill_context_menu(&self, menu: &NSMenu, row: NSInteger) {
        let clicked = self.outline().and_then(|o| o.itemAtRow(row));
        let node = clicked.as_deref().map(node);
        let entries = match node.map(SidebarNode::kind) {
            Some(NodeKind::Request) => REQUEST_MENU,
            Some(NodeKind::Folder) => FOLDER_MENU,
            Some(NodeKind::Group) => HEADER_MENU,
            None => &[],
        };
        let object: Option<&AnyObject> = node.map(AsRef::as_ref);
        // The picker needs the loaded WSDL.
        let schema_ready = self.schema_ready();
        let configure = |item: &NSMenuItem| {
            // SAFETY: a `SidebarNode`, which is what `menu_node` expects back.
            unsafe { item.setRepresentedObject(object) };
            item.setEnabled(item.action() != Some(sel!(sidebarNewRequest:)) || schema_ready);
        };
        // SAFETY: this controller implements every action in the tables and outlives the menu
        // (the outline view owns it).
        unsafe { menu::fill_context_menu(menu, entries, self, configure) };
    }

    /// Selects `node`'s row, as clicking it would, so a command on the selection applies to
    /// it. `false` if the model kept another selection (the previous request could not be
    /// saved).
    fn select_node(&self, node: &SidebarNode) -> bool {
        let key = self.ivars().key;
        let request = node.request();
        self.command("Could not open the request", |app| {
            app.select_request(key, request)
        });
        let selected = self.read_project(key, |w| w.selected_request()).flatten();
        if selected != request {
            self.show_selection();
            return false;
        }
        if let (Some(outline), Some(row)) = (self.outline(), self.row_of(node)) {
            self.applying(|| {
                let rows = NSIndexSet::indexSetWithIndex(row as usize);
                outline.selectRowIndexes_byExtendingSelection(&rows, false);
            });
        }
        true
    }

    /// Deselects every row (and the model's request), for commands on the top level.
    fn select_none(&self) {
        let key = self.ivars().key;
        self.command("Could not open the request", |app| {
            app.select_request(key, None)
        });
        self.show_selection();
    }

    /// Redraws the rows from the model (`SidebarChanged`), keeping collapsed folders
    /// collapsed and the selection. Waits while a name is being edited.
    pub fn reload(&self) {
        if self.is_editing() {
            self.ivars().stale.set(true);
        } else {
            self.reload_now();
        }
    }

    /// Whether a row's name is being edited: the window's first responder is the field
    /// editor of a text field in the outline view.
    pub fn is_editing(&self) -> bool {
        let Some(outline) = self.outline() else {
            return false;
        };
        let Some(responder) = outline.window().and_then(|w| w.firstResponder()) else {
            return false;
        };
        let Ok(editor) = responder.downcast::<NSTextView>() else {
            return false;
        };
        if !editor.isFieldEditor() {
            return false;
        }
        let Some(delegate) = editor.delegate() else {
            return false;
        };
        let delegate: &AnyObject = delegate.as_ref();
        delegate
            .downcast_ref::<NSTextField>()
            .is_some_and(|field| outline.rowForView(field) >= 0)
    }

    fn reload_now(&self) {
        let key = self.ivars().key;
        let Some((sidebar, collapsed)) = self.read_project(key, |window| {
            let collapsed: HashSet<String> = window.collapsed_folders().iter().cloned().collect();
            (window.sidebar().clone(), collapsed)
        }) else {
            return;
        };
        let header_collapsed = self.header_collapsed();
        let selected_folder = self.selected_folder();
        let roots = tree(&sidebar, self.mtm());
        let old = std::mem::replace(&mut *self.ivars().roots.borrow_mut(), roots);
        if let Some(outline) = self.outline() {
            self.applying(|| {
                outline.reloadData();
                // SAFETY: nil expands every root item and, with `true`, all their descendants.
                unsafe { outline.expandItem_expandChildren(None, true) };
                // Children before their parents, so a collapsed folder inside a collapsed one
                // is collapsed too when its parent is expanded again.
                for row in (0..outline.numberOfRows()).rev() {
                    let Some(item) = outline.itemAtRow(row) else {
                        continue;
                    };
                    let n = node(&item);
                    let collapse = match n.kind() {
                        NodeKind::Group => header_collapsed,
                        NodeKind::Folder => n.folder().is_some_and(|p| collapsed.contains(p)),
                        NodeKind::Request => false,
                    };
                    if collapse {
                        // SAFETY: `item` is one of the tree's nodes.
                        unsafe { outline.collapseItem(Some(&item)) };
                    }
                }
            });
        }
        // The outline view no longer refers to the old nodes.
        drop(old);
        self.show_selection_or_folder(selected_folder.as_deref());
    }

    /// Selects the model's selected request (`SelectionChanged`). With no request selected,
    /// a folder the user selected stays selected.
    pub fn show_selection(&self) {
        self.show_selection_or_folder(self.selected_folder().as_deref());
    }

    fn show_selection_or_folder(&self, folder: Option<&str>) {
        let key = self.ivars().key;
        let request = self.read_project(key, |w| w.selected_request()).flatten();
        let Some(outline) = self.outline() else {
            return;
        };
        let row = match request {
            Some(id) => self.row_where(|n| n.request() == Some(id)),
            None => folder.and_then(|path| {
                self.row_where(|n| n.kind() == NodeKind::Folder && n.folder() == Some(path))
            }),
        };
        self.applying(|| match row {
            Some(row) => {
                let rows = NSIndexSet::indexSetWithIndex(row as usize);
                outline.selectRowIndexes_byExtendingSelection(&rows, false);
                outline.scrollRowToVisible(row);
            }
            // SAFETY: `deselectAll:` takes any sender.
            None => unsafe { outline.deselectAll(None) },
        });
    }

    /// Starts inline rename of `request`'s row (`BeginRename`).
    pub fn begin_rename(&self, request: RequestId) {
        self.begin_rename_where(|n| n.request() == Some(request));
    }

    /// Starts inline rename of the folder at `path` (`BeginRenameFolder`).
    pub fn begin_rename_folder(&self, path: &str) {
        self.begin_rename_where(|n| n.kind() == NodeKind::Folder && n.folder() == Some(path));
    }

    fn begin_rename_where(&self, f: impl Fn(&SidebarNode) -> bool) {
        let (Some(outline), Some(row)) = (self.outline(), self.row_where(f)) else {
            return;
        };
        self.applying(|| {
            let rows = NSIndexSet::indexSetWithIndex(row as usize);
            outline.selectRowIndexes_byExtendingSelection(&rows, false);
        });
        outline.scrollRowToVisible(row);
        outline.editColumn_row_withEvent_select(0, row, None, true);
    }

    /// The folder selected in the outline, if a folder row is selected.
    pub fn selected_folder(&self) -> Option<String> {
        let outline = self.outline()?;
        let item = outline.itemAtRow(outline.selectedRow())?;
        let node = node(&item);
        (node.kind() == NodeKind::Folder)
            .then(|| node.folder().map(str::to_owned))
            .flatten()
    }

    /// Where New Folder and a new request go from the selection: the selected folder, else the
    /// selected request's folder, else the top level.
    fn selection_folder(&self) -> String {
        let Some(outline) = self.outline() else {
            return String::new();
        };
        outline
            .itemAtRow(outline.selectedRow())
            .map(|item| node(&item).containing_folder().to_owned())
            .unwrap_or_default()
    }

    /// A request for `operation`, the operation picker's Create: into the selected folder,
    /// else into the operation's folder.
    pub fn new_request(&self, operation: OperationRef) {
        let key = self.ivars().key;
        let folder = self.selected_folder();
        self.command("Could not create a request", |app| {
            app.new_request(key, &operation, folder.as_deref())
        });
    }

    /// File ▸ New Folder: in the selected folder, or the selected request's folder.
    pub fn new_folder(&self) {
        self.new_folder_in(&self.selection_folder());
    }

    fn new_folder_in(&self, parent: &str) {
        let key = self.ivars().key;
        self.command("Could not create a folder", |app| {
            app.new_folder(key, parent)
        });
    }

    /// Whether the WSDL has loaded, so New Request… has operations to pick from.
    pub fn schema_ready(&self) -> bool {
        let key = self.ivars().key;
        self.read_project(key, |w| matches!(w.schema(), SchemaState::Ready(_)))
            .unwrap_or(false)
    }

    /// File ▸ Duplicate, and Duplicate in a request's context menu.
    pub fn duplicate_request(&self, request: RequestId) {
        let key = self.ivars().key;
        self.command("Could not duplicate the request", |app| {
            app.duplicate_request(key, request)
        });
    }

    /// File ▸ Delete, and Delete in a request's context menu.
    pub fn delete_request(&self, request: RequestId) {
        let key = self.ivars().key;
        self.command("Could not delete the request", |app| {
            app.delete_request(key, request)
        });
    }

    /// Delete in a folder's context menu: asks first unless the folder is empty.
    pub fn delete_folder(&self, path: &str) {
        let key = self.ivars().key;
        self.command("Could not delete the folder", |app| {
            app.delete_folder(key, path)
        });
    }

    /// The first row whose node satisfies `f`.
    fn row_where(&self, f: impl Fn(&SidebarNode) -> bool) -> Option<NSInteger> {
        let outline = self.outline()?;
        (0..outline.numberOfRows()).find(|&row| outline.itemAtRow(row).is_some_and(|i| f(node(&i))))
    }

    /// The row node of the folder at `path`; `None` for the top level, which has no row.
    fn folder_node(&self, path: &str) -> Option<Retained<SidebarNode>> {
        let outline = self.outline()?;
        let row = self.row_where(|n| {
            n.kind() == NodeKind::Folder && !path.is_empty() && n.folder() == Some(path)
        })?;
        let item = outline.itemAtRow(row)?;
        item.downcast::<SidebarNode>().ok()
    }

    fn row_of(&self, target: &SidebarNode) -> Option<NSInteger> {
        self.row_where(|n| std::ptr::eq(n, target))
    }

    /// Whether the REQUESTS header is collapsed now. Kept for the window's lifetime only;
    /// folders are kept in the project.
    fn header_collapsed(&self) -> bool {
        let Some(outline) = self.outline() else {
            return false;
        };
        let header = self.ivars().roots.borrow().first().cloned();
        header.is_some_and(|h| {
            let item: &AnyObject = h.as_ref();
            // SAFETY: `item` is one of the tree's nodes.
            !h.children().is_empty() && !unsafe { outline.isItemExpanded(Some(item)) }
        })
    }

    /// A folder's disclosure triangle, clicked by the user: remembered in the project.
    fn disclosure_changed(&self, notification: &NSNotification, collapsed: bool) {
        if self.ivars().applying.get() {
            return;
        }
        let item = notification
            .userInfo()
            .and_then(|info| info.objectForKey(ns_string!("NSObject")));
        let Some(path) = item
            .as_deref()
            .and_then(|i| i.downcast_ref::<SidebarNode>())
            .filter(|n| n.kind() == NodeKind::Folder)
            .and_then(|n| n.folder().map(str::to_owned))
        else {
            return;
        };
        let key = self.ivars().key;
        self.command("Could not remember the folder", |app| {
            app.set_folder_collapsed(key, &path, collapsed)
        });
    }

    /// Whether the click that made `row`'s double action landed on its disclosure triangle.
    fn clicked_disclosure(&self, outline: &SidebarOutlineView, row: NSInteger) -> bool {
        let Some(event) = NSApplication::sharedApplication(self.mtm()).currentEvent() else {
            return false;
        };
        let point = outline.convertPoint_fromView(event.locationInWindow(), None);
        let frame = outline.frameOfOutlineCellAtRow(row);
        point.x >= frame.origin.x
            && point.x <= frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
    }

    /// The folder a drag would drop into, as a node: the proposed folder or header, or the
    /// folder of a request it is over. `None` when the drag is not one of this sidebar's rows
    /// or the model would refuse the move, so the target is not highlighted.
    fn drop_target(
        &self,
        outline: &NSOutlineView,
        info: &ProtocolObject<dyn NSDraggingInfo>,
        item: Option<&AnyObject>,
    ) -> Option<Retained<SidebarNode>> {
        let ours = info.draggingSource().is_some_and(|s| {
            let outline: &AnyObject = outline.as_ref();
            std::ptr::eq(&*s, outline)
        });
        let dragged = self.ivars().dragged.borrow().clone()?;
        if !ours {
            return None;
        }
        let proposed = item?.downcast_ref::<SidebarNode>()?.retain();
        let target = if proposed.kind() == NodeKind::Request {
            let item: &AnyObject = proposed.as_ref();
            // SAFETY: `item` is one of the tree's nodes.
            unsafe { outline.parentForItem(Some(item)) }?
                .downcast::<SidebarNode>()
                .ok()?
        } else {
            proposed
        };
        let folder = target.folder()?;
        let key = self.ivars().key;
        let allowed = self.read_project(key, |window| match &dragged.ivars().data {
            NodeData::Request {
                id, folder: from, ..
            } => from != folder && window.can_move_request(*id, folder),
            NodeData::Folder { path } => {
                let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
                parent != folder && window.can_move_folder(path, folder)
            }
            NodeData::Group => false,
        });
        allowed.unwrap_or(false).then_some(target)
    }

    /// A request or folder row starts a drag. The pasteboard only says that a row is being
    /// dragged; which one is kept here, since the drag never leaves the outline view.
    fn drag_started(
        &self,
        node: &SidebarNode,
    ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
        if node.kind() == NodeKind::Group {
            return None;
        }
        *self.ivars().dragged.borrow_mut() = Some(node.retain());
        let writer = NSPasteboardItem::new();
        writer.setString_forType(&NSString::from_str(&node.title()), drag_type());
        Some(ProtocolObject::from_retained(writer))
    }

    fn validate_drop_on(
        &self,
        outline: &NSOutlineView,
        info: &ProtocolObject<dyn NSDraggingInfo>,
        item: Option<&AnyObject>,
    ) -> NSDragOperation {
        let Some(target) = self.drop_target(outline, info, item) else {
            return NSDragOperation::None;
        };
        // Always onto the folder itself, never between its rows: their order is the name's.
        let target: &AnyObject = target.as_ref();
        // SAFETY: `target` is one of the tree's nodes; -1 means onto the item.
        unsafe {
            outline
                .setDropItem_dropChildIndex(Some(target), NSOutlineViewDropOnItemIndex as NSInteger)
        };
        NSDragOperation::Move
    }

    fn accept_drop_on(
        &self,
        outline: &NSOutlineView,
        info: &ProtocolObject<dyn NSDraggingInfo>,
        item: Option<&AnyObject>,
    ) -> bool {
        let Some(target) = self.drop_target(outline, info, item) else {
            return false;
        };
        let Some(dragged) = self.ivars().dragged.borrow_mut().take() else {
            return false;
        };
        let folder = target.containing_folder().to_owned();
        self.move_node(&dragged, &folder)
    }

    /// Moves the dragged row into `folder` through the model.
    fn move_node(&self, dragged: &SidebarNode, folder: &str) -> bool {
        let key = self.ivars().key;
        match &dragged.ivars().data {
            NodeData::Request { id, .. } => self
                .command("Could not move the request", |app| {
                    app.move_request(key, *id, folder)
                })
                .is_some(),
            NodeData::Folder { path } => self
                .command("Could not move the folder", |app| {
                    app.move_folder(key, path, folder)
                })
                .is_some(),
            NodeData::Group => false,
        }
    }

    fn applying(&self, f: impl FnOnce()) {
        let was = self.ivars().applying.replace(true);
        f();
        self.ivars().applying.set(was);
    }

    fn selection_changed_by_user(&self) {
        let Some(outline) = self.outline() else {
            return;
        };
        let key = self.ivars().key;
        // A folder row selects no request: the editor closes, as for an empty selection.
        let request = outline
            .itemAtRow(outline.selectedRow())
            .and_then(|item| node(&item).request());
        self.command("Could not open the request", |app| {
            app.select_request(key, request)
        });
        // If the model kept its selection (the old request could not be saved), show it.
        self.show_selection();
    }

    fn rename_ended(&self, field: &NSTextField) {
        let Some(outline) = self.outline() else {
            return;
        };
        let row = outline.rowForView(field);
        let Some(item) = outline.itemAtRow(row) else {
            return;
        };
        let node = node(&item);
        let name = field.stringValue().to_string();
        if name == node.title() {
            return;
        }
        let key = self.ivars().key;
        let renamed = match &node.ivars().data {
            NodeData::Request { id, .. } => self
                .command("Could not rename the request", |app| {
                    app.rename_request(key, *id, &name)
                })
                .is_some(),
            NodeData::Folder { path } => self
                .command("Could not rename the folder", |app| {
                    app.rename_folder(key, path, &name)
                })
                .is_some(),
            NodeData::Group => true,
        };
        if !renamed {
            // The field still shows the refused name.
            // SAFETY: `item` is one of the tree's nodes.
            unsafe { outline.reloadItem(Some(&item)) };
        }
    }

    fn row_view(&self, node: &SidebarNode) -> Retained<NSView> {
        let mtm = self.mtm();
        let title = NSString::from_str(&node.title());
        let name = NSTextField::labelWithString(&title, mtm);
        layout::truncating(&name, NSLineBreakMode::ByTruncatingTail);
        if node.kind() == NodeKind::Group {
            return Retained::into_super(layout::cell(&name, Some(&name), mtm));
        }
        name.setEditable(true);
        // SAFETY: the project window controller owns this controller and the window the field
        // lives in, so the controller outlives the field's weak delegate reference.
        unsafe { name.setDelegate(Some(ProtocolObject::from_ref(self))) };
        let mut views = vec![];
        if node.kind() == NodeKind::Folder
            && let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("folder"),
                Some(ns_string!("Folder")),
            )
        {
            let icon = NSImageView::imageViewWithImage(&image, mtm);
            icon.setContentHuggingPriority_forOrientation(
                NSLayoutPriorityDefaultHigh,
                NSLayoutConstraintOrientation::Horizontal,
            );
            views.push(layout::view(icon));
        }
        views.push(layout::view(name.clone()));
        let marker = match node.markers() {
            (_, true) => Some(("⚠", colors::error())),
            (true, false) => Some(("•", NSColor::secondaryLabelColor())),
            (false, false) => None,
        };
        if let Some((text, color)) = marker {
            let marker = layout::small_label(text, mtm);
            marker.setTextColor(Some(&color));
            views.push(layout::view(marker));
        }
        let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        // An icon has no baseline to line up with the name's; centre it instead.
        stack.setAlignment(if node.kind() == NodeKind::Folder {
            NSLayoutAttribute::CenterY
        } else {
            NSLayoutAttribute::FirstBaseline
        });
        // The marker stays in view however long the name: the name gives up width first.
        Retained::into_super(layout::cell(&stack, Some(&name), mtm))
    }
}

impl ModelAccess for SidebarController {}
