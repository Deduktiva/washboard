//! One window per open project (PLAN §8): unified toolbar, sidebar, and a content split of
//! request bar, editor, issues bar and response pane.
//!
//! The controller is an `NSWindowController`, so it sits in the window's responder chain and
//! answers the Project menu while its window is key. One per project open in the model; the
//! app delegate creates and closes it on the model's events, and forwards the events that
//! name its project.

use std::cell::{Cell, OnceCell, RefCell};
use std::ops::Range;
use std::path::Path;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSBeep, NSImage, NSMenuItem, NSMenuItemValidation, NSPopUpButton, NSResponder,
    NSScrollView, NSSplitView, NSSplitViewController, NSSplitViewDividerStyle, NSSplitViewItem,
    NSStackView, NSTextView, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarSidebarTrackingSeparatorItemIdentifier, NSView, NSViewController, NSWindow,
    NSWindowController, NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
    NSWindowToolbarStyle,
};
use objc2_foundation::{
    NSArray, NSCopying, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, ns_string,
};

use washboard_core::model::{RequestId, ServerId};
use washboard_ui_model::{ImportTarget, ModelError, ProjectKey, SchemaState, WellFormedness};

use crate::app::{ModelAccess, with_delegate};
use crate::editor::EditorController;
use crate::layout;
use crate::panes::{IssuesBar, RequestBar, ResponsePane};
use crate::settings_window::{Pane, ServersPane};
use crate::sheets::ImportSheetController;
use crate::sidebar::SidebarController;
use crate::text::replace_summary;

/// Toolbar items of our own, in order: identifier, label, SF Symbol, action.
const TOOLBAR_ITEMS: &[(&str, &str, &str, &str)] = &[
    ("send", "Send", "paperplane", "sendRequest:"),
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
    // No sidebar button: View ▸ Show Sidebar (⌃⌘S) toggles it, as in macOS 26 apps. The
    // server sits right before Send, which goes to it. No Save All: autosave makes it a no-op
    // nearly always, and File ▸ Save All (⌘S) remains. No Validate: requests are validated
    // as they are edited and the issues bar shows the result; Project ▸ Validate (⌘B) skips
    // the wait.
    vec![
        tracking.copy(),
        NSString::from_str(SERVER_ITEM),
        NSString::from_str("send"),
        flexible.copy(),
        NSString::from_str("httpLog"),
    ]
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
    request_bar: RequestBar,
    editor: Retained<EditorController>,
    issues: OnceCell<Retained<IssuesBar>>,
    response: Retained<ResponsePane>,
    split: OnceCell<Retained<NSSplitViewController>>,
    replace: RefCell<Option<Retained<ImportSheetController>>>,
    /// The window the Replace WSDL sheet was attached to, for the alert after it.
    replace_parent: RefCell<Option<Weak<NSWindow>>>,
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

    // SAFETY: `NSMenuItemValidation` has no safety requirements.
    unsafe impl NSMenuItemValidation for ProjectWindowController {
        // SAFETY: the signature matches `validateMenuItem:`.
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            // The menu's Send only sends and Cancel Send only cancels; the toolbar item, which
            // AppKit does not validate here, toggles.
            item.action().is_none_or(|action| self.validates(action))
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
            if let Some(request) = self.selected_request() {
                self.sidebar().duplicate_request(request);
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
            if let Some(request) = self.selected_request() {
                self.sidebar().delete_request(request);
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

        #[unsafe(method(formatXML:))]
        fn format_xml(&self, _sender: Option<&AnyObject>) {
            self.format_request(true);
        }

        #[unsafe(method(validateRequest:))]
        fn validate_request(&self, _sender: Option<&AnyObject>) {
            let key = self.key();
            self.command("Could not validate the request", |app| app.validate(key));
        }

        #[unsafe(method(sendRequest:))]
        fn send_request(&self, _sender: Option<&AnyObject>) {
            // The toolbar's Send is Cancel while a send is in flight.
            if self.is_sending() {
                self.cancel_send();
            } else {
                let key = self.key();
                self.command("Could not send the request", |app| app.send(key));
            }
        }

        #[unsafe(method(cancelSend:))]
        fn cancel_send_action(&self, _sender: Option<&AnyObject>) {
            if self.is_sending() {
                self.cancel_send();
            }
        }

        #[unsafe(method(projectSettings:))]
        fn project_settings(&self, _sender: Option<&AnyObject>) {
            self.show_settings();
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
            request_bar: RequestBar::new(mtm),
            editor: EditorController::for_project(key, mtm),
            issues: OnceCell::new(),
            response: ResponsePane::new(key, mtm),
            split: OnceCell::new(),
            replace: RefCell::new(None),
            replace_parent: RefCell::new(None),
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

    /// Fills the server popup from the model (`ServersChanged`).
    pub fn reload_servers(&self) {
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

    /// The issues bar, ruler and underlines show the editor's issues, and the request bar
    /// whether its text is well-formed (`DiagnosticsChanged`).
    pub fn show_issues(&self) {
        let key = self.key();
        let (issues, basis, state) = self
            .read(|app| {
                app.project(key).and_then(|w| w.editor()).map(|e| {
                    (
                        e.issues().to_vec(),
                        Some(e.issues_basis()),
                        e.well_formedness(),
                    )
                })
            })
            .flatten()
            .unwrap_or((Vec::new(), None, WellFormedness::Pending));
        self.issues().set_issues(issues, basis);
        self.ivars().request_bar.set_state(state);
    }

    /// The request bar shows the open request's name, operation and well-formedness
    /// (`EditorReplaced`, and `SidebarChanged` for a rename).
    pub fn show_request_bar(&self) {
        let key = self.key();
        let summary = self
            .read(|app| app.project(key)?.request_summary())
            .flatten();
        self.ivars().request_bar.show(summary.as_ref());
    }

    pub fn request_bar(&self) -> &RequestBar {
        &self.ivars().request_bar
    }

    /// Format XML (⌃I) on the open request, applied through the editor as one undo step. A
    /// request that is not well-formed is left as it is; its error is in the issues bar, and
    /// with `beep` the user hears that nothing happened.
    pub fn format_request(&self, beep: bool) {
        let key = self.key();
        let selected = self.editor().text_view().selectedRange();
        let selection = Range::from(selected);
        let Some(result) = self.read(|app| app.format_request(key, selection)) else {
            return;
        };
        match result {
            Ok(Some(reformat)) => self.editor().apply_edit(
                reformat.range,
                &reformat.text,
                reformat.selection,
                "Format XML",
            ),
            Ok(None) => {}
            Err(ModelError::NotWellFormed(_)) => {
                if beep {
                    NSBeep();
                }
            }
            Err(e) => {
                with_delegate(self.mtm(), |d| {
                    d.update(|app| app.alert_error("Could not format the request", &e));
                    d.sync();
                });
            }
        }
    }

    /// Whether the menu item sending `action` is enabled while this window is key.
    fn validates(&self, action: Sel) -> bool {
        if action == sel!(formatXML:) {
            self.has_editor()
        } else if action == sel!(deleteRequest:) {
            // ⌘⌫ in a text view deletes to the start of the line. Menu key equivalents are
            // matched before the text view's key bindings, so while one has the focus Delete
            // steps aside and the keystroke reaches it, as in Finder and Mail.
            self.selected_request().is_some() && !self.text_has_focus()
        } else if [
            sel!(duplicateRequest:),
            sel!(renameRequest:),
            sel!(validateRequest:),
        ]
        .contains(&action)
        {
            // These act on the selected request; with none they did nothing.
            self.selected_request().is_some()
        } else if action == sel!(sendRequest:) {
            self.selected_request().is_some() && !self.is_sending()
        } else if action == sel!(cancelSend:) {
            self.is_sending()
        } else if action == sel!(newRequest:) {
            // A request needs an operation from the loaded WSDL.
            self.schema_ready()
        } else {
            true
        }
    }

    /// Whether the window's first responder is a text view: the editor, the response body,
    /// or a field editor (a rename in the sidebar, a text field).
    fn text_has_focus(&self) -> bool {
        self.project_window()
            .firstResponder()
            .is_some_and(|r| r.downcast::<NSTextView>().is_ok())
    }

    /// Whether the WSDL is loaded, so new requests have operations to use.
    fn schema_ready(&self) -> bool {
        let key = self.key();
        self.read(|app| {
            app.project(key)
                .is_some_and(|w| matches!(w.schema(), SchemaState::Ready(_)))
        })
        .unwrap_or(false)
    }

    /// Whether this window's request is being sent.
    pub fn is_sending(&self) -> bool {
        let key = self.key();
        self.read(|app| app.project(key).is_some_and(|w| w.sending()))
            .unwrap_or(false)
    }

    fn cancel_send(&self) {
        let key = self.key();
        with_delegate(self.mtm(), |d| {
            d.update(|app| app.cancel_send(key));
            d.sync();
        });
    }

    fn has_editor(&self) -> bool {
        let key = self.key();
        self.read(|app| app.project(key).is_some_and(|w| w.editor().is_some()))
            .unwrap_or(false)
    }

    /// The model's selected request.
    pub fn selected_request(&self) -> Option<RequestId> {
        let key = self.key();
        self.read(|app| app.project(key).and_then(|w| w.selected_request()))
            .flatten()
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
        let sending = self.is_sending();
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

    /// Project ▸ Project Settings…: the Settings window on this project's Servers pane.
    pub fn show_settings(&self) -> Option<Retained<ServersPane>> {
        let key = self.key();
        with_delegate(self.mtm(), |d| {
            d.show_settings(Some(Pane::Servers(key))).servers(key)
        })
    }

    /// Shows the Replace WSDL sheet on `parent` (the Settings window, whose project pane has
    /// the button), unless it already has a sheet.
    pub fn show_replace_sheet(&self, parent: &NSWindow) -> Option<Retained<ImportSheetController>> {
        if parent.attachedSheet().is_some() {
            return None;
        }
        let target = ImportTarget::ReplaceWsdl(self.key());
        let sheet = ImportSheetController::new(target, self.mtm());
        *self.ivars().replace.borrow_mut() = Some(sheet.clone());
        *self.ivars().replace_parent.borrow_mut() = Some(Weak::from_retained(&parent.retain()));
        sheet.present(parent);
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
        // On the window the sheet was on, where the user is looking; the project window if
        // that one has gone.
        let parent = self
            .ivars()
            .replace_parent
            .borrow()
            .as_ref()
            .and_then(Weak::load)
            .unwrap_or_else(|| self.project_window());
        alert.beginSheetModalForWindow_completionHandler(&parent, None);
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
            self.ivars().request_bar.view(),
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
        let scroll = layout::vertical_scroll(&outline, mtm);
        scroll.setDrawsBackground(false);
        // No buttons under the list: each row has a context menu, and the Project menu has
        // the same commands with shortcuts.
        Retained::into_super(scroll)
    }
}

/// Shared by every project window, so they group into one window's tabs.
pub fn project_tabbing_id() -> &'static NSString {
    ns_string!("WashboardProject")
}

fn window(name: &str, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    let window = layout::owned_window(
        &NSString::from_str(name),
        NSSize::new(1100.0, 700.0),
        style,
        mtm,
    );
    window.setContentMinSize(NSSize::new(720.0, 560.0));
    // Project windows tab with each other (Window ▸ Merge All Windows, or always when the
    // user prefers tabs in System Settings), never with the welcome or New Project window.
    window.setTabbingIdentifier(project_tabbing_id());
    window.setTabbingMode(NSWindowTabbingMode::Automatic);
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
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(140.0, 24.0)),
        false,
    );
    // A popup is as wide as its longest item; a long server name would crowd out the other
    // toolbar items. The menu still shows names in full.
    let width = popup.widthAnchor();
    width
        .constraintGreaterThanOrEqualToConstant(SERVER_POPUP_MIN_WIDTH)
        .setActive(true);
    width
        .constraintLessThanOrEqualToConstant(SERVER_POPUP_MAX_WIDTH)
        .setActive(true);
    popup
}

const SERVER_POPUP_MIN_WIDTH: f64 = 120.0;
const SERVER_POPUP_MAX_WIDTH: f64 = 220.0;

/// The editor (with its issues bar) and the response pane each keep at least this much of the
/// content split; the window's minimum size leaves room for both.
const MIN_EDITOR_HEIGHT: f64 = 260.0;
const MIN_RESPONSE_HEIGHT: f64 = 200.0;

/// Editor with the request bar over it and the issues bar under it, above the response pane.
fn content_pane(
    bar: &NSStackView,
    editor: &NSScrollView,
    issues: &NSStackView,
    response: &NSStackView,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let top = layout::fill_column(
        &[
            Retained::into_super(bar.retain()),
            Retained::into_super(editor.retain()),
            Retained::into_super(issues.retain()),
        ],
        mtm,
    );

    let split = NSSplitView::new(mtm);
    // Horizontal dividers: editor above, response below.
    split.setVertical(false);
    // A hairline, not the thick divider with its dimple.
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.addSubview(&top);
    split.addSubview(response);
    // The split starts at zero size, so without minimums the editor keeps all the height
    // once the window lays out and the response pane is a sliver.
    top.heightAnchor()
        .constraintGreaterThanOrEqualToConstant(MIN_EDITOR_HEIGHT)
        .setActive(true);
    response
        .heightAnchor()
        .constraintGreaterThanOrEqualToConstant(MIN_RESPONSE_HEIGHT)
        .setActive(true);
    split.adjustSubviews();
    Retained::into_super(split)
}

impl ModelAccess for ProjectWindowController {}
