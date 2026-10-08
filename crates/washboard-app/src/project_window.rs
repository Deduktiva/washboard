//! One window per open project (PLAN §8): unified toolbar, sidebar, and a content split of
//! editor, issues bar and response pane.
//!
//! The controller is an `NSWindowController`, so it sits in the window's responder chain and
//! answers the Project menu while its window is key. Handlers are stubs until
//! WP-APP-INTEGRATION binds them to `washboard-ui-model`.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSImage, NSLayoutAttribute, NSPopUpButton,
    NSResponder, NSScrollView, NSSplitView, NSSplitViewController, NSSplitViewItem, NSStackView,
    NSTextField, NSToolbar, NSToolbarDelegate, NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarSidebarTrackingSeparatorItemIdentifier, NSToolbarToggleSidebarItemIdentifier,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController, NSWindow, NSWindowController,
    NSWindowDelegate, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_foundation::{
    NSArray, NSCopying, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, ns_string,
};

use crate::sidebar::SidebarController;

/// Toolbar items of our own, in order: identifier, label, SF Symbol, action.
const TOOLBAR_ITEMS: &[(&str, &str, &str, &str)] = &[
    (
        "validate",
        "Validate",
        "checkmark.circle",
        "validateRequest:",
    ),
    ("send", "Send", "paperplane", "sendRequest:"),
    ("saveAll", "Save All", "square.and.arrow.down", "saveAll:"),
    (
        "httpLog",
        "HTTP Log",
        "list.bullet.rectangle",
        "showHttpLog:",
    ),
];
const SERVER_ITEM: &str = "server";

/// Placeholder until the server list comes from the project.
const SAMPLE_SERVERS: &[&str] = &["Staging", "Production"];

/// The toolbar's identifiers in order; system items first, then ours.
pub fn toolbar_identifiers() -> Vec<Retained<NSString>> {
    // SAFETY: AppKit's identifier constants are immutable statics.
    let (toggle, tracking, flexible) = unsafe {
        (
            NSToolbarToggleSidebarItemIdentifier,
            NSToolbarSidebarTrackingSeparatorItemIdentifier,
            NSToolbarFlexibleSpaceItemIdentifier,
        )
    };
    let mut ids = vec![
        toggle.copy(),
        tracking.copy(),
        NSString::from_str(SERVER_ITEM),
    ];
    ids.extend(
        TOOLBAR_ITEMS[..2]
            .iter()
            .map(|(id, ..)| NSString::from_str(id)),
    );
    ids.push(flexible.copy());
    ids.extend(
        TOOLBAR_ITEMS[2..]
            .iter()
            .map(|(id, ..)| NSString::from_str(id)),
    );
    ids
}

#[derive(Debug)]
pub struct ProjectIvars {
    name: String,
    sidebar: Retained<SidebarController>,
    split: OnceCell<Retained<NSSplitViewController>>,
}

define_class!(
    // SAFETY:
    // - NSWindowController has no subclassing requirements beyond calling a designated
    //   initializer, which `new` does.
    // - `ProjectWindowController` does not implement `Drop`.
    #[unsafe(super(NSWindowController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ProjectIvars]
    #[derive(Debug)]
    pub struct ProjectWindowController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ProjectWindowController {}

    // SAFETY: `NSWindowDelegate` has no safety requirements.
    unsafe impl NSWindowDelegate for ProjectWindowController {
        // SAFETY: the signature matches `windowWillClose:`.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            crate::app::with_delegate(self.mtm(), |app| app.project_closed(self));
        }
    }

    // SAFETY: `NSToolbarDelegate` has no safety requirements.
    unsafe impl NSToolbarDelegate for ProjectWindowController {
        // SAFETY: the signature matches `toolbarDefaultItemIdentifiers:`.
        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            NSArray::from_retained_slice(&toolbar_identifiers())
        }

        // SAFETY: the signature matches `toolbarAllowedItemIdentifiers:`.
        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            NSArray::from_retained_slice(&toolbar_identifiers())
        }

        // SAFETY: the signature matches
        // `toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:`.
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSString,
            _will_insert: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            toolbar_item(identifier, self.mtm())
        }
    }

    // Project menu and toolbar actions, answered while this window is key.
    impl ProjectWindowController {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(newRequest:))]
        fn new_request(&self, _sender: Option<&AnyObject>) {
            self.stub("New Request");
        }

        #[unsafe(method(duplicateRequest:))]
        fn duplicate_request(&self, _sender: Option<&AnyObject>) {
            self.stub("Duplicate");
        }

        #[unsafe(method(renameRequest:))]
        fn rename_request(&self, sender: Option<&AnyObject>) {
            if let Some(outline) = self.ivars().sidebar.outline() {
                // SAFETY: `renameRequest:` takes the sender.
                let _: () = unsafe { msg_send![outline, renameRequest: sender] };
            }
        }

        #[unsafe(method(deleteRequest:))]
        fn delete_request(&self, _sender: Option<&AnyObject>) {
            self.stub("Delete");
        }

        #[unsafe(method(validateRequest:))]
        fn validate_request(&self, _sender: Option<&AnyObject>) {
            self.stub("Validate");
        }

        #[unsafe(method(sendRequest:))]
        fn send_request(&self, _sender: Option<&AnyObject>) {
            self.stub("Send");
        }

        #[unsafe(method(replaceWsdl:))]
        fn replace_wsdl(&self, _sender: Option<&AnyObject>) {
            self.stub("Replace WSDL");
        }

        #[unsafe(method(projectSettings:))]
        fn project_settings(&self, _sender: Option<&AnyObject>) {
            self.stub("Project Settings");
        }

        #[unsafe(method(moreSidebarActions:))]
        fn more_sidebar_actions(&self, _sender: Option<&AnyObject>) {
            self.stub("sidebar ⋯ menu");
        }
    }
);

impl ProjectWindowController {
    pub fn new(name: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let window = window(name, mtm);
        let this = Self::alloc(mtm).set_ivars(ProjectIvars {
            name: name.to_owned(),
            sidebar: SidebarController::new(mtm),
            split: OnceCell::new(),
        });
        // SAFETY: `initWithWindow:` is NSWindowController's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithWindow: &*window] };

        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("ProjectWindow"));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        window.setToolbar(Some(&toolbar));
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);

        let split = this.split_view_controller(mtm);
        window.setContentViewController(Some(&split));
        // `setContentViewController` resizes the window to the controllers' fitting size.
        window.setContentSize(NSSize::new(1100.0, 700.0));
        window.center();
        // The window holds its delegate weakly; this controller owns the window.
        window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        let _ = this.ivars().split.set(split);
        this
    }

    pub fn name(&self) -> &str {
        &self.ivars().name
    }

    pub fn sidebar(&self) -> &SidebarController {
        &self.ivars().sidebar
    }

    pub fn split_view(&self) -> &NSSplitViewController {
        self.ivars().split.get().expect("set in new()")
    }

    pub fn project_window(&self) -> Retained<NSWindow> {
        self.window().expect("created with a window")
    }

    fn stub(&self, what: &str) {
        eprintln!(
            "washboard-app: {what} in {:?} is not implemented yet",
            self.name()
        );
    }

    fn split_view_controller(&self, mtm: MainThreadMarker) -> Retained<NSSplitViewController> {
        let split = NSSplitViewController::new(mtm);

        let sidebar_vc = NSViewController::new(mtm);
        sidebar_vc.setView(&self.sidebar_pane(mtm));
        let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_vc);
        sidebar_item.setMinimumThickness(180.0);
        split.addSplitViewItem(&sidebar_item);

        let content_vc = NSViewController::new(mtm);
        content_vc.setView(&content_pane(mtm));
        let content_item = NSSplitViewItem::splitViewItemWithViewController(&content_vc);
        split.addSplitViewItem(&content_item);
        split
    }

    fn sidebar_pane(&self, mtm: MainThreadMarker) -> Retained<NSView> {
        let outline = self.ivars().sidebar.outline_view(mtm);
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&outline));
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);

        let footer: Vec<Retained<NSView>> = [
            ("+", sel!(newRequest:)),
            ("−", sel!(deleteRequest:)),
            ("⋯", sel!(moreSidebarActions:)),
        ]
        .into_iter()
        .map(|(title, action)| {
            Retained::into_super(Retained::into_super(footer_button(title, action, mtm)))
        })
        .collect();
        let footer = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&footer), mtm);
        footer.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        footer.setSpacing(2.0);

        let views = [Retained::into_super(scroll), Retained::into_super(footer)];
        let pane = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        pane.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        pane.setAlignment(NSLayoutAttribute::Leading);
        pane.setSpacing(0.0);
        Retained::into_super(pane)
    }
}

fn window(name: &str, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1100.0, 700.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    // SAFETY: the designated initializer, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window controller keeps the window, so AppKit must not release it on close
    // as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(name));
    window
}

fn toolbar_item(identifier: &NSString, mtm: MainThreadMarker) -> Option<Retained<NSToolbarItem>> {
    let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), identifier);
    let id = identifier.to_string();
    if id == SERVER_ITEM {
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(140.0, 24.0)),
            false,
        );
        for server in SAMPLE_SERVERS {
            popup.addItemWithTitle(&NSString::from_str(server));
        }
        item.setLabel(ns_string!("Server"));
        item.setView(Some(&popup));
        return Some(item);
    }
    let (_, label, symbol, action) = TOOLBAR_ITEMS.iter().find(|(i, ..)| *i == id)?;
    let label = NSString::from_str(label);
    item.setLabel(&label);
    item.setToolTip(Some(&label));
    item.setImage(
        NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&label),
        )
        .as_deref(),
    );
    item.setBordered(true);
    // SAFETY: no target: the action goes up the responder chain, where every receiver takes
    // the sender as its only argument.
    unsafe { item.setAction(Some(Sel::register(&std::ffi::CString::new(*action).ok()?))) };
    Some(item)
}

fn footer_button(title: &str, action: Sel, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: no target: the action goes up the responder chain to the window controller,
    // whose handlers take the sender as their only argument.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, Some(action), mtm)
    };
    button.setBordered(false);
    button
}

/// Editor, issues bar and response pane. Placeholders: the editor arrives in step 4, the real
/// issues bar and response pane in step 5.
fn content_pane(mtm: MainThreadMarker) -> Retained<NSView> {
    let editor = placeholder("Editor", mtm);
    let issues = NSTextField::labelWithString(
        ns_string!("⚠ 1 error  line 6: 'customerId': '?' is not a valid xs:long"),
        mtm,
    );
    issues.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    issues.setTextColor(Some(&NSColor::systemOrangeColor()));
    let top = [editor, Retained::into_super(Retained::into_super(issues))];
    let top = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&top), mtm);
    top.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    top.setAlignment(NSLayoutAttribute::Leading);

    let split = NSSplitView::new(mtm);
    // Horizontal dividers: editor above, response below.
    split.setVertical(false);
    split.addSubview(&top);
    split.addSubview(&placeholder("Response", mtm));
    split.adjustSubviews();
    Retained::into_super(split)
}

fn placeholder(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    Retained::into_super(Retained::into_super(label))
}
