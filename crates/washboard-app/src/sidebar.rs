//! The project window's source list: REQUESTS and OPERATIONS (service › port › operation),
//! with unsaved (•) and invalid (⚠) markers on requests, inline rename, each port's SOAP
//! version as a chip, and a context menu per row.
//!
//! The rows are the model's [`Sidebar`]; this controller only draws them and turns selection,
//! rename and double-clicks into model commands. `NSOutlineView` identifies rows by object
//! pointer and does not retain its items, so every node is an Objective-C object
//! (`SidebarNode`) owned by the tree in `SidebarController`, rebuilt on `SidebarChanged`.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControl, NSControlTextEditingDelegate, NSEvent, NSLayoutAttribute, NSLineBreakMode,
    NSMenu, NSMenuDelegate, NSMenuItem, NSOutlineView, NSOutlineViewDataSource,
    NSOutlineViewDelegate, NSResponder, NSStackView, NSTableColumn, NSTableView, NSTableViewStyle,
    NSTextField, NSTextFieldDelegate, NSTextView, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString, ns_string,
};
use washboard_core::model::{OperationRef, RequestId};
use washboard_core::wsdl::Protocol;
use washboard_ui_model::{ProjectKey, SchemaState, Sidebar};

use crate::app::{ModelAccess, with_delegate};
use crate::layout;
use crate::menu::menu_item;
use crate::text::port_chip;

/// On a SOAP 1.2 port's row; its operations give their own reason.
fn soap12_tool_tip() -> &'static NSString {
    ns_string!("SOAP 1.2 bindings are not supported")
}

/// What a sidebar row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// The REQUESTS or OPERATIONS header.
    Group,
    Request,
    Service,
    Port,
    Operation,
    /// Stands in for the operations while the WSDL loads, or says why it could not be.
    Placeholder,
}

#[derive(Debug)]
enum NodeData {
    Group,
    Request {
        id: RequestId,
        dirty: bool,
        invalid: bool,
    },
    Service,
    Port {
        protocol: Option<Protocol>,
    },
    Operation {
        operation: OperationRef,
        unsupported: Option<String>,
    },
    Placeholder,
}

#[derive(Debug)]
pub struct NodeIvars {
    data: NodeData,
    title: String,
    /// Names the node across rebuilds, to keep collapsed groups collapsed.
    path: String,
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
        path: String,
        children: Vec<Retained<SidebarNode>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NodeIvars {
            data,
            title: title.to_owned(),
            path,
            children,
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn kind(&self) -> NodeKind {
        match self.ivars().data {
            NodeData::Group => NodeKind::Group,
            NodeData::Request { .. } => NodeKind::Request,
            NodeData::Service => NodeKind::Service,
            NodeData::Port { .. } => NodeKind::Port,
            NodeData::Operation { .. } => NodeKind::Operation,
            NodeData::Placeholder => NodeKind::Placeholder,
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

    /// The (unsaved, invalid) markers of a request row.
    pub fn markers(&self) -> (bool, bool) {
        match self.ivars().data {
            NodeData::Request { dirty, invalid, .. } => (dirty, invalid),
            _ => (false, false),
        }
    }

    /// The operation an operation row stands for.
    pub fn operation(&self) -> Option<&OperationRef> {
        match &self.ivars().data {
            NodeData::Operation { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// A port row's chip: its SOAP version, and "unsupported" for 1.2.
    pub fn chip(&self) -> Option<&'static str> {
        match &self.ivars().data {
            NodeData::Port { protocol } => port_chip(protocol.as_ref()),
            _ => None,
        }
    }

    /// Whether a port row stands for a SOAP 1.2 binding, which Washboard can't send to.
    fn is_soap12(&self) -> bool {
        matches!(
            &self.ivars().data,
            NodeData::Port {
                protocol: Some(Protocol::Soap12)
            }
        )
    }

    /// Why an operation row can't be used, if it can't.
    pub fn unsupported(&self) -> Option<&str> {
        match &self.ivars().data {
            NodeData::Operation { unsupported, .. } => unsupported.as_deref(),
            _ => None,
        }
    }
}

/// The REQUESTS and OPERATIONS groups for `sidebar`; `placeholder` replaces an empty
/// OPERATIONS group.
fn tree(
    sidebar: &Sidebar,
    placeholder: Option<&str>,
    mtm: MainThreadMarker,
) -> Vec<Retained<SidebarNode>> {
    let requests = sidebar
        .requests
        .iter()
        .map(|r| {
            let data = NodeData::Request {
                id: r.id,
                dirty: r.dirty,
                invalid: r.invalid,
            };
            SidebarNode::new(data, &r.name, String::new(), vec![], mtm)
        })
        .collect();
    let mut services: Vec<_> = sidebar
        .services
        .iter()
        .map(|service| {
            let ports = service
                .ports
                .iter()
                .map(|port| {
                    let operations = port
                        .operations
                        .iter()
                        .map(|o| {
                            let data = NodeData::Operation {
                                operation: o.operation.clone(),
                                unsupported: o.unsupported.clone(),
                            };
                            SidebarNode::new(data, o.name(), String::new(), vec![], mtm)
                        })
                        .collect();
                    let path = format!("port:{}/{}", service.name, port.name);
                    let data = NodeData::Port {
                        protocol: port.protocol.clone(),
                    };
                    SidebarNode::new(data, &port.name, path, operations, mtm)
                })
                .collect();
            let path = format!("service:{}", service.name);
            SidebarNode::new(NodeData::Service, &service.name, path, ports, mtm)
        })
        .collect();
    if services.is_empty()
        && let Some(placeholder) = placeholder
    {
        let node = SidebarNode::new(
            NodeData::Placeholder,
            placeholder,
            String::new(),
            vec![],
            mtm,
        );
        services.push(node);
    }
    vec![
        SidebarNode::new(
            NodeData::Group,
            "REQUESTS",
            "group:requests".into(),
            requests,
            mtm,
        ),
        SidebarNode::new(
            NodeData::Group,
            "OPERATIONS",
            "group:ops".into(),
            services,
            mtm,
        ),
    ]
}

/// What the OPERATIONS group says while it has no operations.
fn placeholder(schema: &SchemaState) -> String {
    match schema {
        SchemaState::Loading => "Loading the WSDL…".into(),
        SchemaState::Failed(message) => format!("The WSDL could not be loaded: {message}"),
        SchemaState::Ready(_) => "The WSDL has no services".into(),
    }
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
                // Return renames the selected request, as in Finder.
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
            let row = self.selectedRow();
            let Some(item) = self.itemAtRow(row) else { return };
            if node(&item).kind() == NodeKind::Request {
                self.editColumn_row_withEvent_select(0, row, None, true);
            }
        }
    }
);

#[derive(Debug)]
pub struct SidebarIvars {
    key: ProjectKey,
    roots: RefCell<Vec<Retained<SidebarNode>>>,
    outline: OnceCell<Retained<SidebarOutlineView>>,
    /// Set while the controller changes the outline itself, so its own selection changes
    /// don't go back to the model as commands.
    applying: Cell<bool>,
    /// A reload that arrived while a name was being edited; it would have ended the edit.
    stale: Cell<bool>,
}

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
            matches!(node(item).kind(), NodeKind::Request | NodeKind::Operation)
        }

        // SAFETY: the signature matches `outlineViewSelectionDidChange:`.
        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            if !self.ivars().applying.get() {
                self.selection_changed_by_user();
            }
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
            let operation = menu_node(sender).and_then(|n| n.operation().cloned());
            self.new_request(operation);
        }

        #[unsafe(method(sidebarRenameRequest:))]
        fn context_rename(&self, sender: Option<&AnyObject>) {
            if let Some(request) = self.select_for_menu(sender) {
                self.begin_rename(request);
            }
        }

        #[unsafe(method(sidebarDuplicateRequest:))]
        fn context_duplicate(&self, sender: Option<&AnyObject>) {
            let Some(request) = menu_node(sender).and_then(|n| n.request()) else {
                return;
            };
            let key = self.ivars().key;
            self.command("Could not duplicate the request", |app| {
                app.duplicate_request(key, request)
            });
        }

        #[unsafe(method(sidebarValidateRequest:))]
        fn context_validate(&self, sender: Option<&AnyObject>) {
            if self.select_for_menu(sender).is_some() {
                let key = self.ivars().key;
                self.command("Could not validate the request", |app| app.validate(key));
            }
        }

        #[unsafe(method(sidebarDeleteRequest:))]
        fn context_delete(&self, sender: Option<&AnyObject>) {
            let Some(request) = menu_node(sender).and_then(|n| n.request()) else {
                return;
            };
            let key = self.ivars().key;
            self.command("Could not delete the request", |app| {
                app.delete_request(key, request)
            });
        }
    }

    impl SidebarController {
        // SAFETY: the signature matches an action method; the outline view's double action.
        #[unsafe(method(sidebarDoubleClicked:))]
        fn double_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(outline) = self.outline() else {
                return;
            };
            let Some(item) = outline.itemAtRow(outline.clickedRow()) else {
                return;
            };
            if let Some(operation) = node(&item).operation() {
                self.new_request(Some(operation.clone()));
            }
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

    /// The context menu for `row`: Rename, Duplicate, Validate and Delete on a request; New
    /// Request on an operation and on the REQUESTS header; nothing elsewhere, so no menu opens.
    /// The Project menu keeps the same commands for the selected request, with shortcuts.
    pub fn fill_context_menu(&self, menu: &NSMenu, row: NSInteger) {
        menu.removeAllItems();
        let Some(clicked) = self.outline().and_then(|o| o.itemAtRow(row)) else {
            return;
        };
        let node = node(&clicked);
        let items: &[(&str, Sel)] = match node.kind() {
            NodeKind::Request => &[
                ("Rename", sel!(sidebarRenameRequest:)),
                ("Duplicate", sel!(sidebarDuplicateRequest:)),
                ("Validate", sel!(sidebarValidateRequest:)),
                ("Delete", sel!(sidebarDeleteRequest:)),
            ],
            NodeKind::Operation => &[("New Request", sel!(sidebarNewRequest:))],
            NodeKind::Group if node.ivars().path == "group:requests" => {
                &[("New Request", sel!(sidebarNewRequest:))]
            }
            _ => &[],
        };
        let object: &AnyObject = node.as_ref();
        for &(title, action) in items {
            let item = menu_item(title, Some(action), "", self.mtm());
            // SAFETY: the target is this controller, which implements every action used here
            // and outlives the menu (the outline view owns it); the represented object is a
            // `SidebarNode`, which is what `menu_node` expects back.
            unsafe {
                item.setTarget(Some(self));
                item.setRepresentedObject(Some(object));
            }
            // An unsupported operation gets no request, as a double-click shows.
            item.setEnabled(node.unsupported().is_none());
            menu.addItem(&item);
        }
    }

    /// Selects the context menu's request, as clicking its row would, so a command on the
    /// selected request applies to it. `None` if the model kept another selection (the
    /// previous request could not be saved).
    fn select_for_menu(&self, sender: Option<&AnyObject>) -> Option<RequestId> {
        let request = menu_node(sender)?.request()?;
        let key = self.ivars().key;
        self.command("Could not open the request", |app| {
            app.select_request(key, Some(request))
        });
        self.show_selection(None);
        let selected = self
            .read(|app| app.project(key).and_then(|w| w.selected_request()))
            .flatten();
        (selected == Some(request)).then_some(request)
    }

    /// Redraws the rows from the model (`SidebarChanged`), keeping collapsed groups collapsed
    /// and the selection. Waits while a name is being edited.
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
        let Some((sidebar, placeholder)) = with_delegate(self.mtm(), |d| {
            d.read(|app| {
                let window = app.project(key)?;
                Some((window.sidebar().clone(), placeholder(window.schema())))
            })
        })
        .flatten()
        .flatten() else {
            return;
        };
        let collapsed = self.collapsed_paths();
        let selected_operation = self.selected_operation();
        let roots = tree(&sidebar, Some(&placeholder), self.mtm());
        let old = std::mem::replace(&mut *self.ivars().roots.borrow_mut(), roots);
        if let Some(outline) = self.outline() {
            self.applying(|| {
                outline.reloadData();
                // SAFETY: nil expands every root item and, with `true`, all their descendants.
                unsafe { outline.expandItem_expandChildren(None, true) };
                for row in (0..outline.numberOfRows()).rev() {
                    let Some(item) = outline.itemAtRow(row) else {
                        continue;
                    };
                    if collapsed.contains(&node(&item).ivars().path) {
                        // SAFETY: `item` is one of the tree's nodes.
                        unsafe { outline.collapseItem(Some(&item)) };
                    }
                }
            });
        }
        // The outline view no longer refers to the old nodes.
        drop(old);
        self.show_selection(selected_operation.as_ref());
    }

    /// Selects the model's selected request (`SelectionChanged`). With no request selected,
    /// an operation the user selected stays selected.
    pub fn show_selection(&self, operation: Option<&OperationRef>) {
        let key = self.ivars().key;
        let request = with_delegate(self.mtm(), |d| {
            d.read(|app| app.project(key).and_then(|w| w.selected_request()))
        })
        .flatten()
        .flatten();
        let Some(outline) = self.outline() else {
            return;
        };
        let row = match request {
            Some(id) => self.row_where(|n| n.request() == Some(id)),
            None => operation
                .or(self.selected_operation().as_ref())
                .and_then(|op| self.row_where(|n| n.operation() == Some(op))),
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
        let (Some(outline), Some(row)) = (
            self.outline(),
            self.row_where(|n| n.request() == Some(request)),
        ) else {
            return;
        };
        self.applying(|| {
            let rows = NSIndexSet::indexSetWithIndex(row as usize);
            outline.selectRowIndexes_byExtendingSelection(&rows, false);
        });
        outline.editColumn_row_withEvent_select(0, row, None, true);
    }

    /// The operation selected in the outline, if an operation row is selected.
    pub fn selected_operation(&self) -> Option<OperationRef> {
        let outline = self.outline()?;
        let item = outline.itemAtRow(outline.selectedRow())?;
        node(&item).operation().cloned()
    }

    /// Project ▸ New Request: for `operation`, else the selected operation, else the model's
    /// default.
    pub fn new_request(&self, operation: Option<OperationRef>) {
        let key = self.ivars().key;
        let operation = operation.or_else(|| self.selected_operation());
        self.command("Could not create a request", |app| {
            let operation = match operation {
                Some(operation) => operation,
                None => app.default_operation(key)?,
            };
            app.new_request(key, &operation)
        });
    }

    /// The first row whose node satisfies `f`.
    fn row_where(&self, f: impl Fn(&SidebarNode) -> bool) -> Option<NSInteger> {
        let outline = self.outline()?;
        (0..outline.numberOfRows()).find(|&row| outline.itemAtRow(row).is_some_and(|i| f(node(&i))))
    }

    /// Paths of the expandable rows that are collapsed now.
    fn collapsed_paths(&self) -> HashSet<String> {
        let Some(outline) = self.outline() else {
            return HashSet::new();
        };
        (0..outline.numberOfRows())
            .filter_map(|row| outline.itemAtRow(row))
            .filter(|item| {
                // SAFETY: `item` is one of the tree's nodes.
                !node(item).children().is_empty() && !unsafe { outline.isItemExpanded(Some(item)) }
            })
            .map(|item| node(&item).ivars().path.clone())
            .collect()
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
        let request = match outline.itemAtRow(outline.selectedRow()) {
            Some(item) if node(&item).kind() == NodeKind::Operation => None,
            Some(item) => node(&item).request(),
            None => None,
        };
        self.command("Could not open the request", |app| {
            app.select_request(key, request)
        });
        // If the model kept its selection (the old request could not be saved), show it.
        self.show_selection(None);
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
        let Some(request) = node.request() else {
            return;
        };
        let name = field.stringValue().to_string();
        if name == node.title() {
            return;
        }
        let key = self.ivars().key;
        let renamed = self.command("Could not rename the request", |app| {
            app.rename_request(key, request, &name)
        });
        if renamed.is_none() {
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
        let alone = |name: &NSTextField| Retained::into_super(layout::cell(name, Some(name), mtm));
        match node.kind() {
            NodeKind::Group => return alone(&name),
            NodeKind::Placeholder => {
                name.setTextColor(Some(&NSColor::secondaryLabelColor()));
                name.setToolTip(Some(&title));
                return alone(&name);
            }
            NodeKind::Request => {
                name.setEditable(true);
                // SAFETY: the project window controller owns this controller and the window
                // the field lives in, so the controller outlives the field's weak delegate
                // reference.
                unsafe { name.setDelegate(Some(ProtocolObject::from_ref(self))) };
            }
            NodeKind::Operation => {
                if let Some(why) = node.unsupported() {
                    name.setTextColor(Some(&NSColor::disabledControlTextColor()));
                    name.setToolTip(Some(&NSString::from_str(why)));
                }
            }
            NodeKind::Port if node.is_soap12() => {
                // Greyed like its operations, which say the same in their tool tips.
                name.setTextColor(Some(&NSColor::disabledControlTextColor()));
                name.setToolTip(Some(soap12_tool_tip()));
            }
            NodeKind::Service | NodeKind::Port => {}
        }
        let mut views = vec![layout::view(name.clone())];
        let chip = node.chip().map(|text| layout::chip(text, mtm));
        if let Some((chip, label)) = &chip {
            if node.is_soap12() {
                label.setTextColor(Some(&NSColor::disabledControlTextColor()));
                chip.setToolTip(Some(soap12_tool_tip()));
            }
            views.push(Retained::into_super(chip.clone()));
        }
        let marker = match node.markers() {
            (_, true) => Some(("⚠", NSColor::systemOrangeColor())),
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
        // A box has no baseline to line up with the name's; centre a chip instead.
        stack.setAlignment(if chip.is_some() {
            NSLayoutAttribute::CenterY
        } else {
            NSLayoutAttribute::FirstBaseline
        });
        // The marker stays in view however long the name: the name gives up width first.
        Retained::into_super(layout::cell(&stack, Some(&name), mtm))
    }
}

impl ModelAccess for SidebarController {}
