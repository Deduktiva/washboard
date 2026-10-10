//! One window per open project (PLAN §8): unified toolbar, sidebar, and a content split of the
//! request (request bar, editor, issues bar) beside the response pane (PLAN §4 "Response pane
//! and history"). An older exchange from the history replaces the editor with its request as
//! sent and the toolbar's items with a titlebar accessory.
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
    NSAlert, NSBeep, NSBox, NSBoxType, NSButton, NSColor, NSImage, NSLayoutAttribute,
    NSLineBreakMode, NSMenuItem, NSMenuItemValidation, NSPopUpButton, NSResponder, NSScrollView,
    NSSplitView, NSSplitViewController, NSSplitViewDividerStyle, NSSplitViewItem, NSStackView,
    NSStackViewGravity, NSTextField, NSTextView, NSTitlePosition,
    NSTitlebarAccessoryViewController, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarSidebarTrackingSeparatorItemIdentifier, NSView, NSViewController, NSWindow,
    NSWindowController, NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
    NSWindowToolbarStyle,
};
use objc2_foundation::{
    NSArray, NSCopying, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSUserDefaults, ns_string,
};

use washboard_core::model::{RequestId, ServerId};
use washboard_ui_model::{ImportTarget, ModelError, ProjectKey, SchemaState, WellFormedness};

use crate::app::{ModelAccess, with_delegate};
use crate::editor::EditorController;
use crate::layout;
use crate::panes::{IssuesBar, RequestBar, ResponsePane, date_text, sent_formatter};
use crate::servers_pane::ServersPane;
use crate::settings_window::Pane;
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
    /// An older exchange's request as sent, read-only, in the editor's place.
    sent: Retained<EditorController>,
    issues: OnceCell<Retained<IssuesBar>>,
    response: Retained<ResponsePane>,
    split: OnceCell<Retained<NSSplitViewController>>,
    /// Request beside response.
    content_split: OnceCell<Retained<NSSplitView>>,
    /// "Older exchange · …" under the toolbar, hidden while the latest is shown.
    accessory: OnceCell<Retained<NSTitlebarAccessoryViewController>>,
    older_label: OnceCell<Retained<NSTextField>>,
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
            // An older exchange is never sent or checked by a shortcut, and the editor those
            // act on is out of sight.
            let on_editor = [sel!(sendRequest:), sel!(validateRequest:), sel!(formatXML:)];
            if item.action().is_some_and(|a| on_editor.contains(&a)) && self.older_shown() {
                return false.into();
            }
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

        #[unsafe(method(showLatest:))]
        fn show_latest(&self, _sender: Option<&AnyObject>) {
            self.show_latest_exchange();
        }

        /// Esc from a view that passes it up the responder chain (the History table).
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            if self.older_shown() {
                self.show_latest_exchange();
            }
        }

        #[unsafe(method(restoreRequest:))]
        fn restore_request(&self, _sender: Option<&AnyObject>) {
            self.restore_sent_request();
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
            sent: sent_request_view(mtm),
            issues: OnceCell::new(),
            response: ResponsePane::new(key, mtm),
            split: OnceCell::new(),
            content_split: OnceCell::new(),
            accessory: OnceCell::new(),
            older_label: OnceCell::new(),
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
        // The sidebar's width, kept per project like the frame below.
        split
            .splitView()
            .setAutosaveName(Some(&autosave_name("ProjectSidebar", folder)));
        window.setContentViewController(Some(&split));
        // `setContentViewController` resizes the window to the controllers' fitting size.
        window.setContentSize(NSSize::new(1100.0, 700.0));
        // Size and position are kept per project, by folder (the key changes every launch),
        // as PLAN §3 intends; a project opened for the first time starts centred.
        let frame_name = autosave_name("ProjectWindow", folder);
        if !window.setFrameUsingName(&frame_name) {
            window.center();
        }
        window.setFrameAutosaveName(&frame_name);
        this.add_accessory(&window, mtm);
        // Request and response start with half each, unless the project remembers them.
        window.layoutIfNeeded();
        if let Some(content) = this.ivars().content_split.get() {
            let name = autosave_name("ProjectContent", folder);
            if !has_split_frames(&name) {
                let width = content.frame().size.width;
                content.setPosition_ofDividerAtIndex((width - content.dividerThickness()) / 2.0, 0);
            }
            content.setAutosaveName(Some(&name));
        }
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

    /// An older exchange is shown in place of the latest.
    fn older_shown(&self) -> bool {
        let key = self.key();
        self.read(|app| {
            app.project(key)
                .is_some_and(|w| w.older_exchange().is_some())
        })
        .unwrap_or(false)
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

    /// Shows the latest exchange and the editor, or an older exchange whole: its request as
    /// sent in the editor's place and the accessory instead of the toolbar's items
    /// (`ShownExchangeChanged`). The editor keeps its text, selection and undo stack.
    pub fn show_exchange(&self) {
        let key = self.key();
        let older = self
            .read(|app| {
                let window = app.project(key)?;
                let older = window.older_exchange()?;
                let response = window.response()?;
                let server = window.server_label(response.server, &response.url);
                Some((older.request.clone(), response.sent_at, server))
            })
            .flatten();
        let shown = older.is_some();
        let window = self.project_window();
        if let Some((request, sent_at, server)) = &older {
            let when = date_text(&sent_formatter(), *sent_at);
            self.ivars().sent.set_text(request);
            self.ivars()
                .request_bar
                .show_sent(Some(&format!("Sent {when} · read-only")));
            if let Some(label) = self.ivars().older_label.get() {
                label.setStringValue(&NSString::from_str(&format!(
                    "Older exchange · {when} · {server}"
                )));
            }
        } else {
            self.ivars().request_bar.show_sent(None);
        }
        let leaving_sent = !shown
            && window
                .firstResponder()
                .is_some_and(|r| &*r == self.ivars().sent.text_view().as_ref() as &NSResponder);
        self.editor().view().setHidden(shown);
        self.issues().view().setHidden(shown);
        self.ivars().sent.view().setHidden(!shown);
        if let Some(accessory) = self.ivars().accessory.get() {
            accessory.setHidden(!shown);
        }
        if let Some(toolbar) = window.toolbar() {
            for item in toolbar.items().iter() {
                let id = item.itemIdentifier().to_string();
                if [SERVER_ITEM, "send", "httpLog"].contains(&id.as_str()) {
                    item.setHidden(shown);
                }
            }
        }
        if shown {
            window.makeFirstResponder(Some(self.ivars().sent.text_view()));
        } else if leaving_sent {
            window.makeFirstResponder(Some(self.editor().text_view()));
        }
        self.response().show_history();
    }

    /// Show Latest (Esc): back to the latest exchange and the editor.
    pub fn show_latest_exchange(&self) {
        let key = self.key();
        self.command("Could not show the latest exchange", |app| {
            app.show_latest(key)
        });
    }

    /// Restore Request: puts the older exchange's request into the editor as one undo step,
    /// back on the latest exchange.
    pub fn restore_sent_request(&self) {
        let key = self.key();
        let Some(text) = self.command("Could not restore the request", |app| {
            app.restore_request(key)
        }) else {
            return;
        };
        let editor = self.editor();
        let len = editor.text_view().string().length();
        editor.apply_edit(0..len, &text, 0..0, "Restore Request");
    }

    /// The read-only view of an older exchange's request.
    pub fn sent_request(&self) -> &EditorController {
        &self.ivars().sent
    }

    /// The titlebar accessory shown with an older exchange.
    pub fn older_accessory(&self) -> &NSTitlebarAccessoryViewController {
        self.ivars().accessory.get().expect("set in new()")
    }

    /// The accessory's "Older exchange · <time> · <server>".
    pub fn older_text(&self) -> String {
        self.ivars()
            .older_label
            .get()
            .map(|l| l.stringValue().to_string())
            .unwrap_or_default()
    }

    /// Request beside response.
    pub fn content_split(&self) -> &NSSplitView {
        self.ivars().content_split.get().expect("set in new()")
    }

    /// The bar under the toolbar for an older exchange (PLAN §4): a yellow tint that works in
    /// both appearances, the exchange, Restore Request, and Show Latest on Esc. A unified
    /// toolbar cannot be coloured itself; an accessory is AppKit's way to attach a bar to it.
    fn add_accessory(&self, window: &NSWindow, mtm: MainThreadMarker) {
        let label = layout::small_label("", mtm);
        layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
        let button = |title: &str, action| {
            // SAFETY: this controller owns the window and so the button, which holds its
            // target weakly; both actions take the sender.
            unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(self),
                    Some(action),
                    mtm,
                )
            }
        };
        let restore = button("Restore Request", sel!(restoreRequest:));
        restore.setToolTip(Some(ns_string!("Copy the sent request into the editor")));
        let latest = button("Show Latest", sel!(showLatest:));
        latest.setKeyEquivalent(ns_string!("\u{1b}"));
        latest.setBezelColor(Some(&NSColor::controlAccentColor()));
        latest.setToolTip(Some(ns_string!("Show Latest (Esc)")));
        let bar = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
        bar.addView_inGravity(&label, NSStackViewGravity::Leading);
        bar.addView_inGravity(&restore, NSStackViewGravity::Trailing);
        bar.addView_inGravity(&latest, NSStackViewGravity::Trailing);
        bar.setSpacing(10.0);
        bar.setEdgeInsets(layout::insets(4.0, 12.0, 4.0, 12.0));

        let tint = NSBox::new(mtm);
        tint.setBoxType(NSBoxType::Custom);
        tint.setTitlePosition(NSTitlePosition::NoTitle);
        tint.setBorderWidth(0.0);
        tint.setContentViewMargins(NSSize::new(0.0, 0.0));
        tint.setFillColor(&NSColor::systemYellowColor().colorWithAlphaComponent(0.22));
        tint.setContentView(Some(&bar));
        tint.setFrameSize(NSSize::new(window.frame().size.width, ACCESSORY_HEIGHT));

        let accessory = NSTitlebarAccessoryViewController::new(mtm);
        accessory.setView(&tint);
        accessory.setLayoutAttribute(NSLayoutAttribute::Bottom);
        window.addTitlebarAccessoryViewController(&accessory);
        accessory.setHidden(true);
        let _ = self.ivars().accessory.set(accessory);
        let _ = self.ivars().older_label.set(label);
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

    /// Settings window on this project's Servers pane.
    pub fn show_settings_servers(&self) -> Option<Retained<ServersPane>> {
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
        sidebar_item.setMinimumThickness(SIDEBAR_MIN_WIDTH);
        split.addSplitViewItem(&sidebar_item);

        let content_vc = NSViewController::new(mtm);
        let editor = &self.ivars().editor;
        let issues = IssuesBar::new(editor, mtm);
        let content = content_pane(
            self.ivars().request_bar.view(),
            &[editor.view(), self.ivars().sent.view()],
            issues.view(),
            self.response().view(),
            mtm,
        );
        content_vc.setView(&below_toolbar(&content, mtm));
        // The drawer's split controller manages its items through the view controller tree.
        content_vc.addChildViewController(self.response().drawer());
        let _ = self.ivars().issues.set(issues);
        let _ = self.ivars().content_split.set(content);
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

/// A user defaults name for something kept per project: `kind` and the project's folder.
fn autosave_name(kind: &str, folder: &Path) -> Retained<NSString> {
    NSString::from_str(&format!("{kind} {}", folder.display()))
}

/// A split view with autosave name `name` has saved its subviews' frames before.
fn has_split_frames(name: &NSString) -> bool {
    // AppKit's own key for `NSSplitView.autosaveName`.
    let key = NSString::from_str(&format!("NSSplitView Subview Frames {name}"));
    NSUserDefaults::standardUserDefaults()
        .objectForKey(&key)
        .is_some()
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
    // Room for the sidebar and both halves of the content split at their minimum widths.
    window.setContentMinSize(NSSize::new(
        SIDEBAR_MIN_WIDTH + 2.0 * MIN_HALF_WIDTH + 2.0,
        560.0,
    ));
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

const SIDEBAR_MIN_WIDTH: f64 = 180.0;
/// The request and the response each keep at least this much of the content split.
const MIN_HALF_WIDTH: f64 = 320.0;
const ACCESSORY_HEIGHT: f64 = 32.0;

/// The request side's text views: the editor and the sent-request view, one of them shown.
type RequestViews<'a> = [&'a NSScrollView; 2];

/// The request (bar, text, issues bar) on the left, the response pane on the right.
fn content_pane(
    bar: &NSStackView,
    texts: &RequestViews<'_>,
    issues: &NSStackView,
    response: &NSStackView,
    mtm: MainThreadMarker,
) -> Retained<NSSplitView> {
    let mut views = vec![Retained::into_super(bar.retain())];
    views.extend(texts.iter().map(|t| Retained::into_super(t.retain())));
    views.push(Retained::into_super(issues.retain()));
    // A hidden view leaves the stack (`detachesHiddenViews`), so the shown text takes the
    // height.
    let request = layout::fill_column(&views, mtm);

    let split = NSSplitView::new(mtm);
    // Vertical dividers: request on the left, response on the right.
    split.setVertical(true);
    // A hairline, not the thick divider with its dimple.
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.addSubview(&request);
    split.addSubview(response);
    // The split starts at zero size, so without minimums one half keeps all the width once
    // the window lays out.
    let halves: [&NSView; 2] = [&request, response];
    for half in halves {
        half.widthAnchor()
            .constraintGreaterThanOrEqualToConstant(MIN_HALF_WIDTH)
            .setActive(true);
    }
    split.adjustSubviews();
    split
}

/// `content` in a view of its own, its top pinned to the safe area. The window's content runs
/// under the toolbar (`FullSizeContentView`); scroll views inset their documents by
/// themselves, but the request bar, the status line and the response tabs would be drawn
/// under the toolbar's title and items. The safe area also grows with the older-exchange
/// accessory.
fn below_toolbar(content: &NSView, mtm: MainThreadMarker) -> Retained<NSView> {
    let container = NSView::new(mtm);
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(content);
    let safe = container.safeAreaLayoutGuide();
    for constraint in [
        content
            .topAnchor()
            .constraintEqualToAnchor(&safe.topAnchor()),
        content
            .bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
        content
            .leadingAnchor()
            .constraintEqualToAnchor(&container.leadingAnchor()),
        content
            .trailingAnchor()
            .constraintEqualToAnchor(&container.trailingAnchor()),
    ] {
        constraint.setActive(true);
    }
    container
}

/// A read-only, highlighted text view with line numbers on the window background colour, so
/// it does not read as the editor.
fn sent_request_view(mtm: MainThreadMarker) -> Retained<EditorController> {
    let sent = EditorController::new(mtm);
    let text_view = sent.text_view();
    text_view.setEditable(false);
    text_view.setBackgroundColor(&NSColor::windowBackgroundColor());
    sent.view().setHidden(true);
    sent
}

impl ModelAccess for ProjectWindowController {}
