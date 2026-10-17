//! One window per open project (`docs/gui-drafts.md`): unified toolbar, sidebar, and a content
//! split of the request (request bar, editor, issues bar) beside the response pane
//! (`docs/specs/response-pane.md`). An older exchange from the history replaces the editor with its
//! request as sent and the toolbar's items with a titlebar accessory.
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
    NSAlert, NSBeep, NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor, NSImage,
    NSImageSymbolConfiguration, NSLayoutAttribute, NSLineBreakMode, NSMenuItem,
    NSMenuItemValidation, NSPopUpButton, NSResponder, NSScrollView, NSSplitView,
    NSSplitViewController, NSSplitViewDividerStyle, NSSplitViewItem, NSStackView,
    NSStackViewGravity, NSTextField, NSTextView, NSTitlePosition,
    NSTitlebarAccessoryViewController, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarItem, NSToolbarSidebarTrackingSeparatorItemIdentifier, NSToolbarSpaceItemIdentifier,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController, NSWindow, NSWindowController,
    NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode, NSWindowToolbarStyle,
};
use objc2_foundation::{
    NSArray, NSCopying, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSUserDefaults, ns_string,
};

use washboard_core::model::{ParameterSetId, RequestId, ServerId};
use washboard_ui_model::{ImportTarget, ModelError, ProjectKey, WellFormedness};

use crate::app::{ModelAccess, with_delegate};
use crate::colors;
use crate::editor::EditorController;
use crate::layout;
use crate::operation_picker::OperationPicker;
use crate::panes::{IssuesBar, RequestBar, ResponsePane, date_text, sent_formatter};
use crate::servers_pane::ServersPane;
use crate::settings_window::Pane;
use crate::sheets::ImportSheetController;
use crate::sidebar::SidebarController;
use crate::text::replace_summary;

const SERVER_ITEM: &str = "server";
const PARAMETER_SET_ITEM: &str = "parameterSet";
const SEND_ITEM: &str = "send";

/// A server or set item's tag is its index in the popup's list.
/// The tag of the set popup's "No Parameter Set" item.
pub const NO_SET_ITEM_TAG: NSInteger = -2;
/// The tag of the server popup's "No Servers", which stands for nothing.
const NO_SERVERS_TAG: NSInteger = -3;
/// Separators get a tag of their own: the default, 0, is the first item's, and
/// `selectItemWithTag` picks the first item that has it.
const SEPARATOR_TAG: NSInteger = -4;

#[derive(Debug)]
pub struct ProjectIvars {
    key: ProjectKey,
    name: String,
    /// The model has closed the project (or is closing it); the window follows.
    closing: Cell<bool>,
    sidebar: Retained<SidebarController>,
    toolbar_identifiers: Retained<NSArray<NSString>>,
    /// The toolbar's server popup; its items are `server_ids`, in order.
    servers: Retained<NSPopUpButton>,
    server_ids: RefCell<Vec<ServerId>>,
    /// Before each popup, pins its choice to the selected request or unpins it.
    server_pin: Retained<NSButton>,
    parameter_set_pin: Retained<NSButton>,
    /// The toolbar's parameter set popup: "No Parameter Set", then `parameter_set_ids` in
    /// order. Its toolbar item is hidden while the project has no sets.
    parameter_sets: Retained<NSPopUpButton>,
    parameter_set_ids: RefCell<Vec<ParameterSetId>>,
    parameter_set_item: OnceCell<Retained<NSToolbarItem>>,
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
    picker: RefCell<Option<Retained<OperationPicker>>>,
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
            // Leaving the window saves its edits (`docs/specs/editor.md`).
            let key = self.key();
            self.apply(|app| app.window_resigned_key(key));
        }
    }

    // SAFETY: `NSToolbarDelegate` has no safety requirements.
    unsafe impl NSToolbarDelegate for ProjectWindowController {
        // SAFETY: the signature matches `toolbarDefaultItemIdentifiers:`.
        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            self.ivars().toolbar_identifiers.clone()
        }

        // SAFETY: the signature matches `toolbarAllowedItemIdentifiers:`.
        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            self.ivars().toolbar_identifiers.clone()
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
            self.show_operation_picker();
        }

        #[unsafe(method(newFolder:))]
        fn new_folder(&self, _sender: Option<&AnyObject>) {
            self.sidebar().new_folder();
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
            let tag = self.ivars().servers.selectedTag();
            let key = self.key();
            let index = usize::try_from(tag).ok();
            let server = index.and_then(|i| self.ivars().server_ids.borrow().get(i).copied());
            if let Some(server) = server {
                self.command("Could not choose the server", |app| {
                    app.choose_server(key, server)
                });
            }
            // The popup shows what the model chose, also when it refused.
            self.show_server_selection();
        }

        #[unsafe(method(toggleServerPin:))]
        fn toggle_server_pin(&self, _sender: Option<&AnyObject>) {
            let key = self.key();
            if let Some(pinned) = self.read_project(key, |w| w.selection().server_pinned) {
                self.command("Could not pin the server", |app| {
                    app.set_server_pinned(key, !pinned)
                });
            }
            self.show_server_selection();
        }

        #[unsafe(method(toggleParameterSetPin:))]
        fn toggle_parameter_set_pin(&self, _sender: Option<&AnyObject>) {
            let key = self.key();
            if let Some(pinned) = self.read_project(key, |w| w.selection().parameter_set_pinned) {
                self.command("Could not pin the parameter set", |app| {
                    app.set_parameter_set_pinned(key, !pinned)
                });
            }
            self.show_parameter_set_selection();
        }

        #[unsafe(method(chooseParameterSet:))]
        fn choose_parameter_set(&self, _sender: Option<&AnyObject>) {
            let tag = self.ivars().parameter_sets.selectedTag();
            let key = self.key();
            let set = usize::try_from(tag)
                .ok()
                .and_then(|i| self.ivars().parameter_set_ids.borrow().get(i).copied());
            if set.is_some() || tag == NO_SET_ITEM_TAG {
                self.command("Could not choose the parameter set", |app| {
                    app.choose_parameter_set(key, set)
                });
            }
            self.show_parameter_set_selection();
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

        // The toolbar's identifiers in order; system items first, then ours.
        // SAFETY: AppKit's identifier constants are immutable statics.
        let (tracking, space) = unsafe {
            (
                NSToolbarSidebarTrackingSeparatorItemIdentifier,
                NSToolbarSpaceItemIdentifier,
            )
        };
        // macOS 26 draws adjacent items on one shared glass background; the spaces give the
        // server, the parameter set and Send a background each, so it is clear what is what.
        let toolbar_identifiers = NSArray::from_retained_slice(&[
            tracking.copy(),
            NSString::from_str(SERVER_ITEM),
            space.copy(),
            NSString::from_str(PARAMETER_SET_ITEM),
            space.copy(),
            NSString::from_str(SEND_ITEM),
        ]);

        let this = Self::alloc(mtm).set_ivars(ProjectIvars {
            key,
            name: name.to_owned(),
            closing: Cell::new(false),
            sidebar: SidebarController::new(key, mtm),
            toolbar_identifiers,
            servers: toolbar_popup(mtm),
            server_ids: RefCell::new(Vec::new()),
            server_pin: pin_button(mtm),
            parameter_set_pin: pin_button(mtm),
            parameter_sets: toolbar_popup(mtm),
            parameter_set_ids: RefCell::new(Vec::new()),
            parameter_set_item: OnceCell::new(),
            request_bar: RequestBar::new(mtm),
            editor: EditorController::for_project(key, mtm),
            sent: sent_request_view(mtm),
            issues: OnceCell::new(),
            response: ResponsePane::new(key, mtm),
            split: OnceCell::new(),
            content_split: OnceCell::new(),
            accessory: OnceCell::new(),
            older_label: OnceCell::new(),
            picker: RefCell::new(None),
            replace: RefCell::new(None),
            replace_parent: RefCell::new(None),
            replace_summary: RefCell::new(None),
        });
        // SAFETY: `initWithWindow:` is NSWindowController's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithWindow: &*window] };
        let popups = [
            (&this.ivars().servers, sel!(chooseServer:)),
            (&this.ivars().parameter_sets, sel!(chooseParameterSet:)),
        ];
        for (popup, action) in popups {
            // SAFETY: this controller owns the popup, so it outlives the popup's weak target;
            // both actions take the sender.
            unsafe {
                popup.setTarget(Some(&this));
                popup.setAction(Some(action));
            }
        }
        let pins = [
            (&this.ivars().server_pin, sel!(toggleServerPin:)),
            (
                &this.ivars().parameter_set_pin,
                sel!(toggleParameterSetPin:),
            ),
        ];
        for (button, action) in pins {
            // SAFETY: as for the popups.
            unsafe {
                button.setTarget(Some(&this));
                button.setAction(Some(action));
            }
        }

        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("ProjectWindow"));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        // Icons only, as macOS 26 apps show their toolbars; labels make the unified toolbar
        // tall and crowded.
        toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
        window.setToolbar(Some(&toolbar));
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);
        hide_spaces_after_hidden(&toolbar);

        let split = this.split_view_controller(mtm);
        // The sidebar's width, kept per project like the frame below.
        split
            .splitView()
            .setAutosaveName(Some(&autosave_name("ProjectSidebar", folder)));
        window.setContentViewController(Some(&split));
        // `setContentViewController` resizes the window to the controllers' fitting size.
        window.setContentSize(NSSize::new(1100.0, 700.0));
        // Size and position are kept per project, by folder (the key changes every launch), as
        // `docs/specs/macos-app-state.md` intends; a project opened for the first time starts
        // centred.
        layout::keep_frame(&window, &autosave_name("ProjectWindow", folder));
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

    /// The toolbar's parameter set popup.
    pub fn parameter_set_popup(&self) -> &NSPopUpButton {
        &self.ivars().parameter_sets
    }

    /// The pin button before the server popup.
    pub fn server_pin_button(&self) -> &NSButton {
        &self.ivars().server_pin
    }

    /// The pin button before the parameter set popup.
    pub fn parameter_set_pin_button(&self) -> &NSButton {
        &self.ivars().parameter_set_pin
    }

    /// Whether the toolbar shows the parameter set popup: only while the project has sets.
    pub fn shows_parameter_sets(&self) -> bool {
        self.ivars()
            .parameter_set_item
            .get()
            .is_some_and(|item| !item.isHidden())
    }

    /// Fills the server popup from the model (`ServersChanged`).
    pub fn reload_servers(&self) {
        let key = self.key();
        let servers = self
            .read_project(key, |w| {
                w.servers()
                    .iter()
                    .map(|s| (s.id, s.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let popup = &self.ivars().servers;
        popup.removeAllItems();
        let names: Vec<&str> = servers.iter().map(|(_, name)| name.as_str()).collect();
        add_items(popup, &names);
        if servers.is_empty() {
            add_item(popup, "No Servers", NO_SERVERS_TAG);
        }
        popup.setEnabled(!servers.is_empty());
        *self.ivars().server_ids.borrow_mut() = servers.into_iter().map(|(id, _)| id).collect();
        self.show_server_selection();
    }

    /// Selects the model's server in the popup and shows whether it is pinned
    /// (`ServerSelectionChanged`).
    pub fn show_server_selection(&self) {
        let key = self.key();
        let Some((selected, pinned, request)) = self.read_project(key, |w| {
            let sel = w.selection();
            (
                sel.server,
                sel.server_pinned,
                w.selected_request().is_some(),
            )
        }) else {
            return;
        };
        let index = selected.and_then(|id| {
            self.ivars()
                .server_ids
                .borrow()
                .iter()
                .position(|s| *s == id)
        });
        let popup = &self.ivars().servers;
        if let Some(index) = index.and_then(|i| NSInteger::try_from(i).ok()) {
            popup.selectItemWithTag(index);
        }
        show_pin(&self.ivars().server_pin, pinned, request, "server");
    }

    /// Fills the parameter set popup from the model, and shows it only while the project has
    /// sets (`ParametersChanged`).
    pub fn reload_parameter_sets(&self) {
        let key = self.key();
        let sets = self
            .read_project(key, |w| {
                w.parameter_sets()
                    .iter()
                    .map(|s| (s.id, s.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let popup = &self.ivars().parameter_sets;
        popup.removeAllItems();
        add_item(popup, "No Parameter Set", NO_SET_ITEM_TAG);
        add_separator(popup);
        let names: Vec<&str> = sets.iter().map(|(_, name)| name.as_str()).collect();
        add_items(popup, &names);
        if let Some(item) = self.ivars().parameter_set_item.get() {
            item.setHidden(sets.is_empty());
        }
        if let Some(toolbar) = self.project_window().toolbar() {
            hide_spaces_after_hidden(&toolbar);
        }
        *self.ivars().parameter_set_ids.borrow_mut() = sets.into_iter().map(|(id, _)| id).collect();
        self.show_parameter_set_selection();
    }

    /// Selects the model's parameter set in the popup and shows whether it is pinned
    /// (`ParameterSetSelectionChanged`).
    pub fn show_parameter_set_selection(&self) {
        let key = self.key();
        let Some((selected, pinned, request)) = self.read_project(key, |w| {
            let sel = w.selection();
            (
                sel.parameter_set,
                sel.parameter_set_pinned,
                w.selected_request().is_some(),
            )
        }) else {
            return;
        };
        let index = selected.and_then(|id| {
            self.ivars()
                .parameter_set_ids
                .borrow()
                .iter()
                .position(|s| *s == id)
        });
        let tag = index
            .and_then(|i| NSInteger::try_from(i).ok())
            .unwrap_or(NO_SET_ITEM_TAG);
        let popup = &self.ivars().parameter_sets;
        popup.selectItemWithTag(tag);
        // Without a set there is nothing to pin.
        show_pin(
            &self.ivars().parameter_set_pin,
            pinned,
            request && index.is_some(),
            "parameter set",
        );
    }

    fn toolbar_image(
        &self,
        symbol_name: &NSString,
        label: &NSString,
        color: Retained<NSColor>,
    ) -> Option<Retained<NSImage>> {
        let mut img =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(symbol_name, Some(label));
        if img.is_some() {
            let palette = NSArray::from_retained_slice(&[color]);
            let sym_config = NSImageSymbolConfiguration::configurationWithPaletteColors(&palette);
            img = img?.imageWithSymbolConfiguration(&sym_config);
        }
        img
    }

    fn toolbar_send_item_configure(&self, item: &NSToolbarItem, sending: bool) {
        if sending {
            let label = ns_string!("Cancel");
            item.setLabel(label);
            item.setToolTip(Some(ns_string!("Cancel the running request")));
            item.setImage(
                self.toolbar_image(ns_string!("xmark.circle"), label, NSColor::systemRedColor())
                    .as_deref(),
            );
        } else {
            let label = ns_string!("Send");
            item.setLabel(label);
            item.setToolTip(Some(ns_string!("Send request to server")));
            item.setImage(
                self.toolbar_image(ns_string!("paperplane"), label, NSColor::systemGreenColor())
                    .as_deref(),
            );
        }
    }

    fn toolbar_item(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let mtm = self.mtm();
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), identifier);
        if identifier.to_string() == SEND_ITEM {
            item.setBordered(true);
            self.toolbar_send_item_configure(&item, false);
            // SAFETY: no target: the action goes up the responder chain, where every receiver takes
            // the sender as its only argument.
            unsafe { item.setAction(Some(sel!(sendRequest:))) };
            Some(item)
        } else if identifier.to_string() == SERVER_ITEM {
            item.setLabel(ns_string!("Server"));
            item.setView(Some(&pinned_popup(
                &self.ivars().server_pin,
                &self.ivars().servers,
                self.mtm(),
            )));
            Some(item)
        } else if identifier.to_string() == PARAMETER_SET_ITEM {
            item.setLabel(ns_string!("Parameter Set"));
            item.setView(Some(&pinned_popup(
                &self.ivars().parameter_set_pin,
                &self.ivars().parameter_sets,
                self.mtm(),
            )));
            item.setHidden(self.ivars().parameter_set_ids.borrow().is_empty());
            let _ = self.ivars().parameter_set_item.set(item.clone());
            Some(item)
        } else {
            None
        }
    }

    /// The window's edited dot follows the model (`EditedChanged`).
    pub fn show_edited(&self) {
        let key = self.key();
        let edited = self.read_project(key, |w| w.edited()).unwrap_or(false);
        self.project_window().setDocumentEdited(edited);
    }

    /// The issues bar, ruler and underlines show the editor's issues, and the request bar
    /// whether its text is well-formed (`DiagnosticsChanged`).
    pub fn show_issues(&self) {
        let key = self.key();
        let (issues, basis, state) = self
            .read_project(key, |w| {
                w.editor().map(|e| {
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
        let summary = self.read_project(key, |w| w.request_summary()).flatten();
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
                self.apply(|app| app.alert_error("Could not format the request", &e));
            }
        }
    }

    /// An older exchange is shown in place of the latest.
    fn older_shown(&self) -> bool {
        self.read_project(self.key(), |w| w.older_exchange().is_some())
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
            self.sidebar().schema_ready()
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

    /// Shows the latest exchange and the editor, or an older exchange whole: its request as
    /// sent in the editor's place and the accessory instead of the toolbar's items
    /// (`ShownExchangeChanged`). The editor keeps its text, selection and undo stack.
    pub fn show_exchange(&self) {
        let key = self.key();
        let older = self
            .read_project(key, |window| {
                let older = window.older_exchange()?;
                let response = window.response()?;
                let server =
                    window.exchange_label(response.server, &response.url, response.parameter_set);
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
        // Hidden views lose the focus to the window; it moves only off what is being hidden, so
        // a pick in the History table keeps ↑/↓ there.
        let focus_in = |view: &NSView| {
            window
                .firstResponder()
                .and_then(|r| r.downcast::<NSView>().ok())
                .is_some_and(|r| r.isDescendantOf(view))
        };
        let leaving_editor =
            shown && (focus_in(self.editor().view()) || focus_in(self.issues().view()));
        let leaving_sent = !shown && focus_in(self.ivars().sent.view());
        self.editor().view().setHidden(shown);
        self.issues().view().setHidden(shown);
        self.ivars().sent.view().setHidden(!shown);
        if let Some(accessory) = self.ivars().accessory.get() {
            accessory.setHidden(!shown);
        }
        if let Some(toolbar) = window.toolbar() {
            for item in toolbar.items().iter() {
                let id = item.itemIdentifier().to_string();
                if [SERVER_ITEM, SEND_ITEM].contains(&id.as_str()) {
                    item.setHidden(shown);
                } else if id == PARAMETER_SET_ITEM {
                    // Back from an older exchange, it shows only while there are sets.
                    let none = self.ivars().parameter_set_ids.borrow().is_empty();
                    item.setHidden(shown || none);
                }
            }
            hide_spaces_after_hidden(&toolbar);
        }
        if leaving_editor {
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

    /// The bar under the toolbar for an older exchange (`docs/specs/response-pane.md`): a yellow
    /// tint that works in both appearances, the exchange, Restore Request, and Show Latest on Esc.
    /// A unified toolbar cannot be coloured itself; an accessory is AppKit's way to attach a bar to
    /// it.
    fn add_accessory(&self, window: &NSWindow, mtm: MainThreadMarker) {
        let label = layout::small_label("", mtm);
        layout::truncating(&label, NSLineBreakMode::ByTruncatingTail);
        let restore = layout::button("Restore Request", self, sel!(restoreRequest:), mtm);
        restore.setToolTip(Some(ns_string!("Copy the sent request into the editor")));
        let latest = layout::button("Show Latest", self, sel!(showLatest:), mtm);
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
        self.read_project(self.key(), |w| w.sending())
            .unwrap_or(false)
    }

    fn cancel_send(&self) {
        let key = self.key();
        self.apply(|app| app.cancel_send(key));
    }

    fn has_editor(&self) -> bool {
        self.read_project(self.key(), |w| w.editor().is_some())
            .unwrap_or(false)
    }

    /// The model's selected request.
    pub fn selected_request(&self) -> Option<RequestId> {
        self.read_project(self.key(), |w| w.selected_request())
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
            self.toolbar_send_item_configure(&item, sending);
        }
        self.response().show_response();
    }

    /// The toolbar's Send (or Cancel) item.
    pub fn send_item(&self) -> Option<Retained<NSToolbarItem>> {
        let toolbar = self.project_window().toolbar()?;
        toolbar
            .items()
            .iter()
            .find(|i| i.itemIdentifier().to_string() == SEND_ITEM)
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
        // Shows the model's sheet through `ImportChanged`.
        self.apply(|app| app.begin_import(target));
        Some(sheet)
    }

    /// New Request…: the operation picker on this window, unless the WSDL has not loaded or
    /// the window already has a sheet. It starts on the selected request's operation.
    pub fn show_operation_picker(&self) -> Option<Retained<OperationPicker>> {
        let window = self.project_window();
        if window.attachedSheet().is_some() || !self.sidebar().schema_ready() {
            return None;
        }
        let key = self.key();
        let (services, request_operation) = self.read_project(key, |window| {
            let operation = window.request_summary().and_then(|r| r.operation);
            (window.sidebar().services.clone(), operation)
        })?;
        let picker = OperationPicker::new(key, services, request_operation, self.mtm());
        picker.present(&window);
        *self.ivars().picker.borrow_mut() = Some(picker.clone());
        Some(picker)
    }

    /// The most recent operation picker, if one was shown.
    pub fn operation_picker(&self) -> Option<Retained<OperationPicker>> {
        self.ivars().picker.borrow().clone()
    }

    /// The most recent Replace WSDL sheet, if one was shown.
    pub fn replace_sheet(&self) -> Option<Retained<ImportSheetController>> {
        self.ivars().replace.borrow().clone()
    }

    /// After Replace WSDL (`WsdlReplaced`): says what changed.
    pub fn show_replace_outcome(&self) {
        let key = self.key();
        let Some(summary) = self
            .read_project(key, |w| w.replace_outcome().map(replace_summary))
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

/// The server or parameter set popup.
fn toolbar_popup(mtm: MainThreadMarker) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(140.0, 24.0)),
        false,
    );
    // A popup is as wide as its longest item; a long name would crowd out the other toolbar
    // items. The menu still shows names in full.
    let width = popup.widthAnchor();
    width
        .constraintGreaterThanOrEqualToConstant(POPUP_MIN_WIDTH)
        .setActive(true);
    width
        .constraintLessThanOrEqualToConstant(POPUP_MAX_WIDTH)
        .setActive(true);
    // The pin item is enabled by `show_pin`, not by the responder chain.
    if let Some(menu) = popup.menu() {
        menu.setAutoenablesItems(false);
    }
    popup
}

/// Adds an item per name, tagged with its index.
fn add_items(popup: &NSPopUpButton, names: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        add_item(
            popup,
            name,
            NSInteger::try_from(i).unwrap_or(NSInteger::MAX),
        );
    }
}

fn add_item(popup: &NSPopUpButton, title: &str, tag: NSInteger) {
    // Not `addItemWithTitle`: it replaces an item with the same title, and two servers may
    // share a name, and a separator's title is the empty string a placeholder title would be.
    let Some(menu) = popup.menu() else { return };
    // SAFETY: no action; the popup sends its own when the item is chosen.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(popup.mtm()),
            &NSString::from_str(title),
            None,
            ns_string!(""),
        )
    };
    item.setTag(tag);
    menu.addItem(&item);
}

fn add_separator(popup: &NSPopUpButton) {
    if let Some(menu) = popup.menu() {
        let separator = NSMenuItem::separatorItem(popup.mtm());
        separator.setTag(SEPARATOR_TAG);
        menu.addItem(&separator);
    }
}

/// A borderless button showing whether the popup after it is pinned to the request.
fn pin_button(mtm: MainThreadMarker) -> Retained<NSButton> {
    let button = NSButton::new(mtm);
    button.setBordered(false);
    button.setImagePosition(NSCellImagePosition::ImageOnly);
    button
}

/// A space follows an item only while that item shows: without the parameter sets, the server
/// and Send would otherwise stand two spaces apart.
fn hide_spaces_after_hidden(toolbar: &NSToolbar) {
    // SAFETY: AppKit's identifier constants are immutable statics.
    let space = unsafe { NSToolbarSpaceItemIdentifier };
    let mut previous_hidden = false;
    for item in toolbar.items().iter() {
        if *item.itemIdentifier() == *space {
            item.setHidden(previous_hidden);
        } else {
            previous_hidden = item.isHidden();
        }
    }
}

/// The pin button and its popup side by side, as one toolbar item's view.
fn pinned_popup(
    pin: &NSButton,
    popup: &NSPopUpButton,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let views = [layout::view(pin.retain()), layout::view(popup.retain())];
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    // SF Symbols are drawn to sit on a text baseline, like a letter: centred on its frame, the
    // pin hung below the popup's title.
    stack.setAlignment(NSLayoutAttribute::FirstBaseline);
    stack.setSpacing(4.0);
    stack
}

/// The pin is always visible and clickable (`docs/specs/parameters.md`, "Selection"): an
/// outline pin while the request follows the project, a filled one in the warning colour while
/// it is pinned. It is enabled only when there is a request (and a `what`) to pin.
fn show_pin(button: &NSButton, pinned: bool, can_pin: bool, what: &str) {
    let (symbol, label, tint, tip) = if pinned {
        (
            ns_string!("pin.fill"),
            ns_string!("Pinned"),
            colors::warning(),
            format!("Pinned to this request. Click to use the project's {what}."),
        )
    } else {
        (
            ns_string!("pin"),
            ns_string!("Not pinned"),
            NSColor::secondaryLabelColor(),
            format!("The project's {what}. Click to pin it to this request."),
        )
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(symbol, Some(label));
    button.setImage(image.as_deref());
    button.setContentTintColor(Some(&tint));
    button.setEnabled(pinned || can_pin);
    button.setToolTip(Some(&NSString::from_str(&tip)));
}

const POPUP_MIN_WIDTH: f64 = 120.0;
const POPUP_MAX_WIDTH: f64 = 220.0;

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
