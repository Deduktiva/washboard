//! One window per open project (PLAN §8): unified toolbar, sidebar, and a content split of
//! editor, issues bar and response pane.
//!
//! The controller is an `NSWindowController`, so it sits in the window's responder chain and
//! answers the Project menu while its window is key. One per project open in the model; the
//! app delegate creates and closes it on the model's events, and forwards the events that
//! name its project.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSBackingStoreType, NSButton, NSImage, NSLayoutAttribute, NSMenu, NSMenuItem,
    NSPopUpButton, NSResponder, NSScrollView, NSSplitView, NSSplitViewController, NSSplitViewItem,
    NSStackView, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarSidebarTrackingSeparatorItemIdentifier, NSUserInterfaceLayoutOrientation, NSView,
    NSViewController, NSWindow, NSWindowController, NSWindowDelegate, NSWindowStyleMask,
    NSWindowToolbarStyle,
};
use objc2_foundation::{
    NSArray, NSCopying, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, ns_string,
};

use washboard_core::model::{RequestId, ServerId};
use washboard_ui_model::{App, ImportTarget, ModelError, ProjectKey};

use crate::app::with_delegate;
use crate::editor::EditorController;
use crate::panes::{IssuesBar, ResponsePane};
use crate::sheets::{ImportSheetController, SettingsSheet};
use crate::sidebar::SidebarController;
use crate::text::replace_summary;

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

/// The toolbar's identifiers in order; system items first, then ours.
pub fn toolbar_identifiers() -> Vec<Retained<NSString>> {
    // SAFETY: AppKit's identifier constants are immutable statics.
    let (tracking, flexible) = unsafe {
        (
            NSToolbarSidebarTrackingSeparatorItemIdentifier,
            NSToolbarFlexibleSpaceItemIdentifier,
        )
    };
    // No sidebar button: View ▸ Show Sidebar (⌃⌘S) toggles it, as in macOS 26 apps.
    let mut ids = vec![tracking.copy(), NSString::from_str(SERVER_ITEM)];
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
    key: ProjectKey,
    name: String,
    /// The model has closed the project (or is closing it); the window follows.
    closing: Cell<bool>,
    sidebar: Retained<SidebarController>,
    /// The toolbar's server popup; its items are `server_ids`, in order.
    servers: Retained<NSPopUpButton>,
    server_ids: RefCell<Vec<ServerId>>,
    editor: Retained<EditorController>,
    issues: OnceCell<Retained<IssuesBar>>,
    response: Retained<ResponsePane>,
    split: OnceCell<Retained<NSSplitViewController>>,
    settings: OnceCell<Retained<SettingsSheet>>,
    replace: RefCell<Option<Retained<ImportSheetController>>>,
    /// What the last Replace WSDL changed, as shown in its alert.
    replace_summary: RefCell<Option<String>>,
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
        // SAFETY: the signature matches `windowShouldClose:`.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            crate::app::with_delegate(self.mtm(), |app| app.close_requested(self)).unwrap_or(true)
        }

        // SAFETY: the signature matches `windowWillClose:`.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // `close` skips `windowShouldClose:`; the model must still let the project go.
            if !self.is_closing() {
                crate::app::with_delegate(self.mtm(), |app| {
                    app.close_requested(self);
                });
            }
        }

        // SAFETY: the signature matches `windowDidResignKey:`.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            // Leaving the window saves its edits (PLAN §4 "Save / autosave").
            let key = self.key();
            with_delegate(self.mtm(), |d| {
                d.update(|app| app.window_resigned_key(key));
                d.sync();
            });
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
            self.toolbar_item(identifier)
        }
    }

    // Project menu and toolbar actions, answered while this window is key.
    impl ProjectWindowController {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(newRequest:))]
        fn new_request(&self, _sender: Option<&AnyObject>) {
            self.sidebar().new_request(None);
        }

        #[unsafe(method(duplicateRequest:))]
        fn duplicate_request(&self, _sender: Option<&AnyObject>) {
            let key = self.key();
            if let Some(request) = self.selected_request() {
                self.command("Could not duplicate the request", |app| {
                    app.duplicate_request(key, request)
                });
            }
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
            let key = self.key();
            if let Some(request) = self.selected_request() {
                self.command("Could not delete the request", |app| {
                    app.delete_request(key, request)
                });
            }
        }

        #[unsafe(method(chooseServer:))]
        fn choose_server(&self, _sender: Option<&AnyObject>) {
            let index = usize::try_from(self.ivars().servers.indexOfSelectedItem()).ok();
            let server = index.and_then(|i| self.ivars().server_ids.borrow().get(i).copied());
            let key = self.key();
            if let Some(server) = server {
                self.command("Could not choose the server", |app| {
                    app.choose_server(key, server)
                });
            }
            // The popup shows what the model chose, also when it refused.
            self.show_server_selection();
        }

        #[unsafe(method(validateRequest:))]
        fn validate_request(&self, _sender: Option<&AnyObject>) {
            let key = self.key();
            self.command("Could not validate the request", |app| app.validate(key));
        }

        #[unsafe(method(sendRequest:))]
        fn send_request(&self, _sender: Option<&AnyObject>) {
            // The toolbar's Send is Cancel while a send is in flight.
            let key = self.key();
            let sending = self
                .read(|app| app.project(key).is_some_and(|w| w.sending()))
                .unwrap_or(false);
            if sending {
                with_delegate(self.mtm(), |d| {
                    d.update(|app| app.cancel_send(key));
                    d.sync();
                });
            } else {
                self.command("Could not send the request", |app| app.send(key));
            }
        }

        #[unsafe(method(replaceWsdl:))]
        fn replace_wsdl(&self, _sender: Option<&AnyObject>) {
            self.show_replace_sheet();
        }

        #[unsafe(method(projectSettings:))]
        fn project_settings(&self, _sender: Option<&AnyObject>) {
            self.show_settings();
        }

        #[unsafe(method(moreSidebarActions:))]
        fn more_sidebar_actions(&self, sender: Option<&AnyObject>) {
            let Some(button) = sender.and_then(|s| s.downcast_ref::<NSView>()) else {
                return;
            };
            // Just below the button, like a pull-down.
            let at = NSPoint::new(0.0, button.bounds().size.height + 4.0);
            sidebar_actions_menu(self.mtm()).popUpMenuPositioningItem_atLocation_inView(
                None,
                at,
                Some(button),
            );
        }
    }
);

impl ProjectWindowController {
    pub fn new(
        key: ProjectKey,
        name: &str,
        folder: &Path,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let window = window(name, mtm);
        window.setRepresentedFilename(&NSString::from_str(&folder.display().to_string()));
        let this = Self::alloc(mtm).set_ivars(ProjectIvars {
            key,
            name: name.to_owned(),
            closing: Cell::new(false),
            sidebar: SidebarController::new(key, mtm),
            servers: server_popup(mtm),
            server_ids: RefCell::new(Vec::new()),
            editor: EditorController::for_project(key, mtm),
            issues: OnceCell::new(),
            response: ResponsePane::new(key, mtm),
            split: OnceCell::new(),
            settings: OnceCell::new(),
            replace: RefCell::new(None),
            replace_summary: RefCell::new(None),
        });
        // SAFETY: `initWithWindow:` is NSWindowController's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithWindow: &*window] };
        let popup = &this.ivars().servers;
        // SAFETY: this controller owns the popup, so it outlives the popup's weak target;
        // `chooseServer:` takes the sender.
        unsafe {
            popup.setTarget(Some(&this));
            popup.setAction(Some(sel!(chooseServer:)));
        }

        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("ProjectWindow"));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        // Icons only, as macOS 26 apps show their toolbars; labels make the unified toolbar
        // tall and crowded.
        toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
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

    /// The model's project this window shows.
    pub fn key(&self) -> ProjectKey {
        self.ivars().key
    }

    pub fn name(&self) -> &str {
        &self.ivars().name
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.ivars().closing.get()
    }

    pub(crate) fn set_closing(&self, closing: bool) {
        self.ivars().closing.set(closing);
    }

    pub fn sidebar(&self) -> &SidebarController {
        &self.ivars().sidebar
    }

    /// The toolbar's server popup.
    pub fn server_popup(&self) -> &NSPopUpButton {
        &self.ivars().servers
    }

    /// Fills the server popup, and the settings sheet if shown, from the model
    /// (`ServersChanged`).
    pub fn reload_servers(&self) {
        if let Some(settings) = self.settings() {
            settings.reload();
        }
        let key = self.key();
        let servers = self
            .read(|app| {
                app.project(key).map(|w| {
                    w.servers()
                        .iter()
                        .map(|s| (s.id, s.name.clone()))
                        .collect::<Vec<_>>()
                })
            })
            .flatten()
            .unwrap_or_default();
        let popup = &self.ivars().servers;
        popup.removeAllItems();
        for (_, name) in &servers {
            // Not `addItemWithTitle`: it drops an item whose title is already in the menu.
            popup.addItemWithTitle(ns_string!(""));
            if let Some(item) = popup.lastItem() {
                item.setTitle(&NSString::from_str(name));
            }
        }
        if servers.is_empty() {
            popup.addItemWithTitle(ns_string!("No Servers"));
        }
        popup.setEnabled(!servers.is_empty());
        *self.ivars().server_ids.borrow_mut() = servers.into_iter().map(|(id, _)| id).collect();
        self.show_server_selection();
    }

    /// Selects the model's server in the popup (`ServerSelectionChanged`).
    pub fn show_server_selection(&self) {
        let key = self.key();
        let selected = self
            .read(|app| app.project(key).and_then(|w| w.selected_server()))
            .flatten();
        let index = selected.and_then(|id| {
            self.ivars()
                .server_ids
                .borrow()
                .iter()
                .position(|s| *s == id)
        });
        if let Some(index) = index.and_then(|i| NSInteger::try_from(i).ok()) {
            self.ivars().servers.selectItemAtIndex(index);
        }
    }

    fn toolbar_item(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let mtm = self.mtm();
        if identifier.to_string() != SERVER_ITEM {
            return toolbar_item(identifier, mtm);
        }
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), identifier);
        item.setLabel(ns_string!("Server"));
        item.setView(Some(&self.ivars().servers));
        Some(item)
    }

    /// The window's edited dot follows the model (`EditedChanged`).
    pub fn show_edited(&self) {
        let key = self.key();
        let edited = self
            .read(|app| app.project(key).is_some_and(|w| w.edited()))
            .unwrap_or(false);
        self.project_window().setDocumentEdited(edited);
    }

    /// The issues bar, ruler and underlines show the editor's issues (`DiagnosticsChanged`).
    pub fn show_issues(&self) {
        let key = self.key();
        let issues = self
            .read(|app| {
                app.project(key)
                    .and_then(|w| w.editor())
                    .map(|e| e.issues().to_vec())
            })
            .flatten()
            .unwrap_or_default();
        self.issues().set_issues(issues);
    }

    /// The model's selected request.
    pub fn selected_request(&self) -> Option<RequestId> {
        let key = self.key();
        self.read(|app| app.project(key).and_then(|w| w.selected_request()))
            .flatten()
    }

    fn read<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        with_delegate(self.mtm(), |d| d.read(f)).flatten()
    }

    fn command<R>(&self, title: &str, f: impl FnOnce(&mut App) -> Result<R, ModelError>) {
        with_delegate(self.mtm(), |d| d.command(title, f));
    }

    pub fn editor(&self) -> &EditorController {
        &self.ivars().editor
    }

    pub fn issues(&self) -> &IssuesBar {
        self.ivars().issues.get().expect("set in new()")
    }

    pub fn response(&self) -> &ResponsePane {
        &self.ivars().response
    }

    /// Send ↔ Cancel in the toolbar, and the response pane's status (`SendStateChanged`).
    pub fn show_send_state(&self) {
        let key = self.key();
        let sending = self
            .read(|app| app.project(key).is_some_and(|w| w.sending()))
            .unwrap_or(false);
        if let Some(item) = self.send_item() {
            let (label, symbol) = if sending {
                ("Cancel", "xmark.circle")
            } else {
                ("Send", "paperplane")
            };
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
        }
        self.response().show_response();
    }

    /// The toolbar's Send (or Cancel) item.
    pub fn send_item(&self) -> Option<Retained<NSToolbarItem>> {
        let toolbar = self.project_window().toolbar()?;
        toolbar
            .items()
            .iter()
            .find(|i| i.itemIdentifier().to_string() == "send")
    }

    /// Shows the project's settings sheet on its window, on the Servers tab.
    pub fn show_settings(&self) -> &SettingsSheet {
        let sheet = self
            .ivars()
            .settings
            .get_or_init(|| SettingsSheet::new(self.key(), self.name(), self.mtm()));
        let window = self.project_window();
        if window.attachedSheet().is_none() {
            sheet.reload();
            sheet.present(&window);
        }
        sheet
    }

    /// The settings sheet, once shown.
    pub fn settings(&self) -> Option<&SettingsSheet> {
        self.ivars().settings.get().map(|s| &**s)
    }

    /// Shows the Replace WSDL sheet, unless the window already has a sheet.
    pub fn show_replace_sheet(&self) -> Option<Retained<ImportSheetController>> {
        let window = self.project_window();
        if window.attachedSheet().is_some() {
            return None;
        }
        let target = ImportTarget::ReplaceWsdl(self.key());
        let sheet = ImportSheetController::new(target, self.mtm());
        *self.ivars().replace.borrow_mut() = Some(sheet.clone());
        sheet.present(&window);
        with_delegate(self.mtm(), |d| {
            d.update(|app| app.begin_import(target));
            // Shows the model's sheet through `ImportChanged`.
            d.sync();
        });
        Some(sheet)
    }

    /// The most recent Replace WSDL sheet, if one was shown.
    pub fn replace_sheet(&self) -> Option<Retained<ImportSheetController>> {
        self.ivars().replace.borrow().clone()
    }

    /// After Replace WSDL (`WsdlReplaced`): says what changed. The full report is
    /// WP-REPLACE-REPORT's.
    pub fn show_replace_outcome(&self) {
        let key = self.key();
        let Some(summary) = self
            .read(|app| app.project(key)?.replace_outcome().map(replace_summary))
            .flatten()
        else {
            return;
        };
        *self.ivars().replace_summary.borrow_mut() = Some(summary.clone());
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(ns_string!("The WSDL was replaced"));
        alert.setInformativeText(&NSString::from_str(&summary));
        alert.beginSheetModalForWindow_completionHandler(&self.project_window(), None);
    }

    /// The text of the last Replace WSDL alert.
    pub fn replace_summary(&self) -> Option<String> {
        self.ivars().replace_summary.borrow().clone()
    }

    pub fn split_view(&self) -> &NSSplitViewController {
        self.ivars().split.get().expect("set in new()")
    }

    pub fn project_window(&self) -> Retained<NSWindow> {
        self.window().expect("created with a window")
    }

    fn split_view_controller(&self, mtm: MainThreadMarker) -> Retained<NSSplitViewController> {
        let split = NSSplitViewController::new(mtm);

        let sidebar_vc = NSViewController::new(mtm);
        sidebar_vc.setView(&self.sidebar_pane(mtm));
        let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_vc);
        sidebar_item.setMinimumThickness(180.0);
        split.addSplitViewItem(&sidebar_item);

        let content_vc = NSViewController::new(mtm);
        let editor = &self.ivars().editor;
        let issues = IssuesBar::new(editor, mtm);
        content_vc.setView(&content_pane(
            editor.view(),
            issues.view(),
            self.response().view(),
            mtm,
        ));
        let _ = self.ivars().issues.set(issues);
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

fn server_popup(mtm: MainThreadMarker) -> Retained<NSPopUpButton> {
    NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(140.0, 24.0)),
        false,
    )
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

/// Editor with the issues bar under it, above the response pane.
/// The sidebar's ⋯ menu: the request commands without a footer button of their own. The
/// items go up the responder chain like the Project menu's, so they validate the same way.
pub fn sidebar_actions_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    for (title, action) in [
        ("Rename", sel!(renameRequest:)),
        ("Duplicate", sel!(duplicateRequest:)),
        ("Validate", sel!(validateRequest:)),
    ] {
        // SAFETY: no target: the action goes up the responder chain to this window's
        // controller, whose handlers take the sender as their only argument.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                ns_string!(""),
            )
        };
        menu.addItem(&item);
    }
    menu
}

fn content_pane(
    editor: &NSScrollView,
    issues: &NSStackView,
    response: &NSStackView,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let top: [Retained<NSView>; 2] = [
        Retained::into_super(editor.retain()),
        Retained::into_super(issues.retain()),
    ];
    let top = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&top), mtm);
    top.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    top.setAlignment(NSLayoutAttribute::Leading);
    top.setSpacing(0.0);

    let split = NSSplitView::new(mtm);
    // Horizontal dividers: editor above, response below.
    split.setVertical(false);
    split.addSubview(&top);
    split.addSubview(response);
    split.adjustSubviews();
    Retained::into_super(split)
}
