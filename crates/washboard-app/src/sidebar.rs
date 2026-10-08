//! The project window's source list: REQUESTS and OPERATIONS (service › port › operation),
//! with unsaved (•) and invalid (⚠) markers on requests and inline rename.
//!
//! `NSOutlineView` identifies rows by object pointer and does not retain its items, so every
//! node is an Objective-C object (`SidebarNode`) owned by the tree in `SidebarController`.

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSControl, NSControlTextEditingDelegate, NSEvent, NSFont, NSLayoutAttribute,
    NSOutlineView, NSOutlineViewDataSource, NSOutlineViewDelegate, NSResponder, NSStackView,
    NSTableColumn, NSTableView, NSTableViewStyle, NSTextField, NSTextFieldDelegate,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    NSArray, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString, ns_string,
};

/// What a sidebar row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// The REQUESTS or OPERATIONS header.
    Group,
    Request,
    Service,
    Port,
    Operation,
}

/// Sample data shaped like the core types until `washboard-ui-model` supplies the tree.
#[derive(Debug, Clone)]
pub struct SampleRequest {
    pub name: &'static str,
    pub unsaved: bool,
    pub invalid: bool,
}

pub const SAMPLE_REQUESTS: &[SampleRequest] = &[
    SampleRequest {
        name: "GetCustomer 1",
        unsaved: false,
        invalid: false,
    },
    SampleRequest {
        name: "GetCustomer 2",
        unsaved: true,
        invalid: false,
    },
    SampleRequest {
        name: "CreateOrder 1",
        unsaved: false,
        invalid: true,
    },
];

/// service › port › operations.
pub const SAMPLE_OPERATIONS: &[(&str, &str, &[&str])] = &[(
    "CustomerService",
    "CustomerPort",
    &["GetCustomer", "CreateOrder", "ListOrders"],
)];

#[derive(Debug)]
pub struct NodeIvars {
    kind: NodeKind,
    title: RefCell<String>,
    unsaved: bool,
    invalid: bool,
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
        kind: NodeKind,
        title: &str,
        children: Vec<Retained<SidebarNode>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        Self::with_markers(kind, title, false, false, children, mtm)
    }

    fn with_markers(
        kind: NodeKind,
        title: &str,
        unsaved: bool,
        invalid: bool,
        children: Vec<Retained<SidebarNode>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NodeIvars {
            kind,
            title: RefCell::new(title.to_owned()),
            unsaved,
            invalid,
            children,
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn kind(&self) -> NodeKind {
        self.ivars().kind
    }

    pub fn title(&self) -> String {
        self.ivars().title.borrow().clone()
    }

    pub fn children(&self) -> &[Retained<SidebarNode>] {
        &self.ivars().children
    }
}

fn sample_tree(mtm: MainThreadMarker) -> Vec<Retained<SidebarNode>> {
    let requests = SAMPLE_REQUESTS
        .iter()
        .map(|r| {
            SidebarNode::with_markers(NodeKind::Request, r.name, r.unsaved, r.invalid, vec![], mtm)
        })
        .collect();
    let services = SAMPLE_OPERATIONS
        .iter()
        .map(|(service, port, operations)| {
            let operations = operations
                .iter()
                .map(|o| SidebarNode::new(NodeKind::Operation, o, vec![], mtm))
                .collect();
            let port = SidebarNode::new(NodeKind::Port, port, operations, mtm);
            SidebarNode::new(NodeKind::Service, service, vec![port], mtm)
        })
        .collect();
    vec![
        SidebarNode::new(NodeKind::Group, "REQUESTS", requests, mtm),
        SidebarNode::new(NodeKind::Group, "OPERATIONS", services, mtm),
    ]
}

/// The node behind an outline item. Every item the outline view hands back is one of ours.
fn node(item: &AnyObject) -> &SidebarNode {
    item.downcast_ref::<SidebarNode>()
        .expect("sidebar items are SidebarNodes")
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
    roots: Vec<Retained<SidebarNode>>,
    outline: OnceCell<Retained<SidebarOutlineView>>,
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
                None => self.ivars().roots.len() as NSInteger,
                Some(item) => node(item).children().len() as NSInteger,
            }
        }

        // SAFETY: the signature matches `outlineView:child:ofItem:`; the child is retained by
        // the tree, which outlives the outline view's use of it.
        #[unsafe(method_id(outlineView:child:ofItem:))]
        fn child(
            &self,
            _outline: &NSOutlineView,
            index: NSInteger,
            item: Option<&AnyObject>,
        ) -> Retained<AnyObject> {
            let children = match item {
                None => &self.ivars().roots[..],
                Some(item) => node(item).children(),
            };
            let index = usize::try_from(index).expect("AppKit asks for a valid index");
            Retained::into_super(Retained::into_super(children[index].clone()))
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
            // Rename is local until `washboard-ui-model` takes the command.
            let Some(field) = notification.object() else {
                return;
            };
            let Some(field) = field.downcast_ref::<NSTextField>() else {
                return;
            };
            let Some(outline) = self.outline() else {
                return;
            };
            let view: &NSView = field;
            let row = outline.rowForView(view);
            let Some(item) = outline.itemAtRow(row) else {
                return;
            };
            let name = field.stringValue().to_string();
            if !name.trim().is_empty() {
                *node(&item).ivars().title.borrow_mut() = name;
            }
            // SAFETY: `item` is one of the tree's nodes.
            unsafe { outline.reloadItem(Some(&item)) };
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
    }
);

impl SidebarController {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SidebarIvars {
            roots: sample_tree(mtm),
            outline: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn roots(&self) -> &[Retained<SidebarNode>] {
        &self.ivars().roots
    }

    /// The outline view, once `outline_view` has built it.
    pub fn outline(&self) -> Option<&SidebarOutlineView> {
        self.ivars().outline.get().map(|o| &**o)
    }

    /// Builds the outline view with this controller as data source and delegate, and
    /// expands everything.
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
        // SAFETY: the project window controller owns this controller and the outline view's
        // window, so the controller outlives the outline view's weak references to it.
        unsafe {
            outline.setDataSource(Some(ProtocolObject::from_ref(self)));
            outline.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        // SAFETY: nil expands every root item and, with `true`, all their descendants.
        unsafe { outline.expandItem_expandChildren(None, true) };
        let _ = self.ivars().outline.set(outline.clone());
        outline
    }

    fn row_view(&self, node: &SidebarNode) -> Retained<NSView> {
        let mtm = self.mtm();
        let title = NSString::from_str(&node.title());
        if node.kind() == NodeKind::Group {
            let label = NSTextField::labelWithString(&title, mtm);
            return Retained::into_super(Retained::into_super(label));
        }
        let name = NSTextField::labelWithString(&title, mtm);
        if node.kind() == NodeKind::Request {
            name.setEditable(true);
            // SAFETY: the project window controller owns this controller and the window the
            // field lives in, so the controller outlives the field's weak delegate reference.
            unsafe { name.setDelegate(Some(ProtocolObject::from_ref(self))) };
        }
        let mut views = vec![Retained::into_super(Retained::into_super(name))];
        let marker = match (node.ivars().unsaved, node.ivars().invalid) {
            (_, true) => Some(("⚠", NSColor::systemOrangeColor())),
            (true, false) => Some(("•", NSColor::secondaryLabelColor())),
            (false, false) => None,
        };
        if let Some((text, color)) = marker {
            let marker = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            marker.setTextColor(Some(&color));
            marker.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            views.push(Retained::into_super(Retained::into_super(marker)));
        }
        let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        stack.setAlignment(NSLayoutAttribute::FirstBaseline);
        Retained::into_super(stack)
    }
}
