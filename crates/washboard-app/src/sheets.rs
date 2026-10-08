//! The sheets of PLAN §8: the import sheet (New Project and Replace WSDL) bound to the
//! model's `ImportSheet`, and Project Settings › Servers bound to the project's servers.
//! The model runs the import check and stores everything; the sheets show its state and
//! forward input.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBezelStyle, NSBorderType, NSButton, NSControlStateValue,
    NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate,
    NSGridCellPlacement, NSGridRowAlignment, NSGridView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh, NSLayoutPriorityDefaultLow,
    NSLineBreakMode, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSPathControl, NSPathStyle,
    NSSecureTextField, NSStackView, NSStackViewDistribution, NSStackViewGravity, NSTabView,
    NSTabViewItem, NSTextField, NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexSet, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSURL, ns_string,
};
use washboard_core::model::{Auth, Server, ServerId};
use washboard_ui_model::{App, CheckState, ImportSheet, ImportTarget, ProjectKey, SuggestedServer};

use crate::app::{ModelAccess, with_delegate};
use crate::layout;
use crate::table::TextTable;
use crate::text::{import_messages, import_status, reference_row};

/// The ✓/✗ columns: wide enough for the mark, so the reference text gets the room.
const MARK_WIDTH: f64 = 22.0;
/// The import sheet's tables: the references grow with the window, the findings do not.
const MIN_REFERENCES_HEIGHT: f64 = 120.0;
const MESSAGES_HEIGHT: f64 = 90.0;
/// The Servers tab's list column.
const LIST_WIDTH: f64 = 180.0;
const TIMEOUT_WIDTH: f64 = 60.0;
const SQUARE_BUTTON: f64 = 24.0;
/// About three rows and the header of the suggested servers.
const SUGGESTIONS_HEIGHT: f64 = 90.0;

/// The import sheet's controls.
#[derive(Debug)]
struct ImportViews {
    window: Retained<NSWindow>,
    name: Retained<NSTextField>,
    location: Retained<NSPathControl>,
    wsdl: Retained<NSPathControl>,
    files: Retained<NSTextField>,
    add: Retained<NSButton>,
    clear: Retained<NSButton>,
    references: Retained<TextTable>,
    messages: Retained<TextTable>,
    status: Retained<NSTextField>,
    finish: Retained<NSButton>,
}

#[derive(Debug)]
pub struct ImportIvars {
    target: ImportTarget,
    views: OnceCell<ImportViews>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `ImportSheetController` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ImportIvars]
    #[derive(Debug)]
    pub struct ImportSheetController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for ImportSheetController {}

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for ImportSheetController {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for ImportSheetController {
        // SAFETY: the signature matches `controlTextDidChange:`.
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.name_changed();
        }
    }

    // SAFETY: `NSWindowDelegate` has no safety requirements.
    unsafe impl NSWindowDelegate for ImportSheetController {
        // SAFETY: the signature matches `windowWillClose:`.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.closed();
        }
    }

    impl ImportSheetController {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(cancel:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) {
            self.cancel();
        }

        #[unsafe(method(finish:))]
        fn finish_action(&self, _sender: Option<&AnyObject>) {
            self.finish();
        }

        #[unsafe(method(chooseLocation:))]
        fn choose_location_action(&self, _sender: Option<&AnyObject>) {
            self.open_panel(false, true, false, |sheet, mut paths| {
                sheet.choose_location(paths.remove(0));
            });
        }

        #[unsafe(method(chooseWsdl:))]
        fn choose_wsdl_action(&self, _sender: Option<&AnyObject>) {
            self.open_panel(true, false, false, |sheet, mut paths| {
                sheet.choose_wsdl(paths.remove(0));
            });
        }

        #[unsafe(method(addFiles:))]
        fn add_files_action(&self, _sender: Option<&AnyObject>) {
            self.open_panel(true, true, true, |sheet, paths| sheet.add_files(paths));
        }

        #[unsafe(method(clearFiles:))]
        fn clear_files_action(&self, _sender: Option<&AnyObject>) {
            self.clear_files();
        }
    }
);

impl ImportSheetController {
    /// The model's sheet for `target` must exist (`App::begin_import`); the controller shows it
    /// on every [`reload`](Self::reload) and ends itself once the model drops it.
    pub fn new(target: ImportTarget, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ImportIvars {
            target,
            views: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let new_project = target == ImportTarget::NewProject;
        let name = NSTextField::textFieldWithString(ns_string!(""), mtm);
        name.setPlaceholderString(Some(ns_string!("Project name")));
        // SAFETY: this object owns the field through its window, so it outlives the field's
        // weak delegate reference.
        unsafe { name.setDelegate(Some(ProtocolObject::from_ref(&*this))) };
        let value = || {
            let field = NSTextField::labelWithString(ns_string!(""), mtm);
            field.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
            shrinkable(&field);
            field
        };
        let path = || {
            // Folder icons and names, shortened in the middle when space runs out, as Xcode
            // and Finder show a location, instead of a label as wide as the path.
            let control = NSPathControl::new(mtm);
            control.setPathStyle(NSPathStyle::Standard);
            control.setPlaceholderString(Some(ns_string!("Not chosen")));
            shrinkable(&control);
            control
        };
        let (location, wsdl, files) = (path(), path(), value());
        let button = |title: &str, action: Sel| target_button(title, &this, action, mtm);
        let add = button("Add…", sel!(addFiles:));
        let clear = button("Clear", sel!(clearFiles:));

        let mut rows = Vec::new();
        if new_project {
            rows.push(vec![label("Name:", mtm), view(name.clone())]);
            rows.push(vec![
                label("Location:", mtm),
                value_row(
                    vec![
                        view(location.clone()),
                        view(button("Choose…", sel!(chooseLocation:))),
                    ],
                    mtm,
                ),
            ]);
        }
        rows.push(vec![
            label("WSDL:", mtm),
            value_row(
                vec![
                    view(wsdl.clone()),
                    view(button("Choose…", sel!(chooseWsdl:))),
                ],
                mtm,
            ),
        ]);
        rows.push(vec![
            label("XSD files:", mtm),
            value_row(
                vec![view(files.clone()), view(add.clone()), view(clear.clone())],
                mtm,
            ),
        ]);
        let form = grid(rows, mtm);

        let references = TextTable::new(&["", "Reference", "Resolved to"], mtm);
        let messages = TextTable::new(&["", "Message"], mtm);
        let status = NSTextField::labelWithString(ns_string!(""), mtm);
        layout::truncating(&status, NSLineBreakMode::ByTruncatingTail);
        let cancel = button("Cancel", sel!(cancel:));
        cancel.setKeyEquivalent(ns_string!("\u{1b}"));
        let finish = button(
            if new_project { "Create" } else { "Replace" },
            sel!(finish:),
        );
        finish.setKeyEquivalent(ns_string!("\r"));
        let buttons = trailing_row(vec![view(cancel), view(finish.clone())], mtm);
        references.fix_column_width(0, MARK_WIDTH);
        messages.fix_column_width(0, MARK_WIDTH);
        // The references take the spare height; the findings keep a few rows.
        references.view().setBorderType(NSBorderType::BezelBorder);
        messages.view().setBorderType(NSBorderType::BezelBorder);
        references
            .view()
            .heightAnchor()
            .constraintGreaterThanOrEqualToConstant(MIN_REFERENCES_HEIGHT)
            .setActive(true);
        layout::set_height(messages.view(), MESSAGES_HEIGHT);

        let content = window_content(
            vec![
                form,
                view(references.view().retain()),
                view(messages.view().retain()),
                view(status.clone()),
            ],
            buttons,
            mtm,
        );
        let title = if new_project {
            "New Project"
        } else {
            "Replace WSDL"
        };
        let size = NSSize::new(640.0, 560.0);
        let window = sheet_window(title, size, mtm);
        if new_project {
            // Its own window, so that open projects stay usable meanwhile.
            window.setStyleMask(
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
            );
            window.setContentMinSize(size);
            window.setTabbingMode(NSWindowTabbingMode::Disallowed);
            window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        }
        window.setContentView(Some(&content));

        let _ = this.ivars().views.set(ImportViews {
            window,
            name,
            location,
            wsdl,
            files,
            add,
            clear,
            references,
            messages,
            status,
            finish,
        });
        this
    }

    pub fn target(&self) -> ImportTarget {
        self.ivars().target
    }

    pub fn window(&self) -> &NSWindow {
        &self.views().window
    }

    pub fn name_field(&self) -> &NSTextField {
        &self.views().name
    }

    /// The chosen project location.
    pub fn location(&self) -> &NSPathControl {
        &self.views().location
    }

    /// One row per reference: mark, reference as written, resolved file.
    pub fn references(&self) -> &TextTable {
        &self.views().references
    }

    /// The import check's and schema compile's findings.
    pub fn messages(&self) -> &TextTable {
        &self.views().messages
    }

    pub fn status(&self) -> String {
        self.views().status.stringValue().to_string()
    }

    /// Create or Replace.
    pub fn finish_button(&self) -> &NSButton {
        &self.views().finish
    }

    /// Attaches Replace WSDL to its project's window.
    pub fn present(&self, parent: &NSWindow) {
        parent.beginSheet_completionHandler(self.window(), None);
    }

    /// Shows New Project as a window of its own, centred on the screen.
    pub fn show(&self) {
        self.window().center();
        self.window().makeKeyAndOrderFront(None);
    }

    /// Shows the model's sheet, or ends this one once the model has dropped it (Cancel,
    /// Create, Replace).
    pub fn reload(&self) {
        let target = self.target();
        let shown = self
            .read(|app| {
                app.import_sheet(target)
                    .map(|sheet| Shown::new(sheet, target))
            })
            .flatten();
        let Some(shown) = shown else {
            dismiss(self.window());
            return;
        };
        let views = self.views();
        if views.name.stringValue().to_string() != shown.name {
            views.name.setStringValue(&NSString::from_str(&shown.name));
        }
        let set = |field: &NSTextField, text: &str| field.setStringValue(&NSString::from_str(text));
        let set_path = |control: &NSPathControl, path: Option<&Path>| {
            let url =
                path.map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())));
            control.setURL(url.as_deref());
        };
        set_path(&views.location, shown.location.as_deref());
        set_path(&views.wsdl, shown.wsdl.as_deref());
        set(&views.files, &shown.files);
        set(&views.status, &shown.status);
        views.add.setEnabled(shown.has_entry);
        views.clear.setEnabled(shown.has_extra);
        views.references.set_rows(shown.references);
        views.messages.set_rows(shown.messages);
        views.finish.setEnabled(shown.can_finish);
    }

    /// Types `name` into the Name field.
    pub fn set_name(&self, name: &str) {
        self.name_field().setStringValue(&NSString::from_str(name));
        self.name_changed();
    }

    /// The folder the new project is created in.
    pub fn choose_location(&self, parent: PathBuf) {
        let name = self.name_field().stringValue().to_string();
        self.command("Could not set the location", move |app| {
            app.set_import_destination(&name, Some(parent))
        });
    }

    /// The entry WSDL; restarts the check.
    pub fn choose_wsdl(&self, entry: PathBuf) {
        let extra = self.files().1;
        self.set_files(entry, extra);
    }

    /// More XSD/WSDL files or folders; restarts the check. Needs a WSDL first.
    pub fn add_files(&self, paths: Vec<PathBuf>) {
        let (entry, mut extra) = self.files();
        let Some(entry) = entry else {
            return;
        };
        for path in paths {
            if !extra.contains(&path) {
                extra.push(path);
            }
        }
        self.set_files(entry, extra);
    }

    pub fn clear_files(&self) {
        if let Some(entry) = self.files().0 {
            self.set_files(entry, Vec::new());
        }
    }

    pub fn cancel(&self) {
        let target = self.target();
        with_delegate(self.mtm(), |d| {
            d.update(|app| app.cancel_import(target));
            d.sync();
        });
        // Also when the model had no sheet left to cancel.
        dismiss(self.window());
    }

    /// Create or Replace; also called by tests. Does nothing while disabled. A new project
    /// whose WSDL names server addresses opens with its settings, to confirm them.
    pub fn finish(&self) {
        if !self.finish_button().isEnabled() {
            return;
        }
        match self.target() {
            ImportTarget::NewProject => {
                let Some(key) = self.command("Could not create the project", App::create_project)
                else {
                    return;
                };
                with_delegate(self.mtm(), |d| {
                    let suggested = d
                        .read(|app| app.project(key).map(|w| !w.suggested_servers().is_empty()))
                        .flatten()
                        .unwrap_or(false);
                    if let Some(project) = d.project(key)
                        && suggested
                    {
                        project.show_settings();
                    }
                });
            }
            ImportTarget::ReplaceWsdl(key) => {
                self.command("Could not replace the WSDL", |app| app.replace_wsdl(key));
            }
        }
    }

    fn views(&self) -> &ImportViews {
        self.ivars().views.get().expect("set in new()")
    }

    /// The New Project window's close button means Cancel. Create and Cancel order the window
    /// out rather than closing it, so the model only still has the sheet when the user closed
    /// the window.
    fn closed(&self) {
        let target = self.target();
        if self.read(|app| app.import_sheet(target).is_some()) == Some(true) {
            self.cancel();
        }
    }

    fn name_changed(&self) {
        let name = self.name_field().stringValue().to_string();
        let parent = self
            .read(|app| app.import_sheet(ImportTarget::NewProject)?.parent.clone())
            .flatten();
        self.command("Could not set the name", move |app| {
            app.set_import_destination(&name, parent)
        });
    }

    /// The model's entry WSDL and extra files.
    fn files(&self) -> (Option<PathBuf>, Vec<PathBuf>) {
        let target = self.target();
        self.read(|app| {
            app.import_sheet(target)
                .map(|s| (s.entry.clone(), s.extra.clone()))
        })
        .flatten()
        .unwrap_or_default()
    }

    fn set_files(&self, entry: PathBuf, extra: Vec<PathBuf>) {
        let target = self.target();
        self.command("Could not check the files", move |app| {
            app.set_import_files(target, entry, extra)
        });
    }

    /// An open panel as a sheet on this sheet; `then` gets the chosen paths, never none.
    fn open_panel(
        &self,
        files: bool,
        directories: bool,
        multiple: bool,
        then: impl Fn(&Self, Vec<PathBuf>) + 'static,
    ) {
        let panel = NSOpenPanel::openPanel(self.mtm());
        panel.setCanChooseFiles(files);
        panel.setCanChooseDirectories(directories);
        panel.setCanCreateDirectories(directories && !files);
        panel.setAllowsMultipleSelection(multiple);
        let chosen = panel.clone();
        let this = self.retain();
        let handler = RcBlock::new(move |response: NSModalResponse| {
            if response != NSModalResponseOK {
                return;
            }
            let paths: Vec<PathBuf> = chosen
                .URLs()
                .iter()
                .filter_map(|url| url.to_file_path())
                .collect();
            if !paths.is_empty() {
                then(&this, paths);
            }
        });
        panel.beginSheetModalForWindow_completionHandler(self.window(), &handler);
    }
}

/// What the import sheet shows, read from the model in one borrow.
struct Shown {
    name: String,
    location: Option<PathBuf>,
    wsdl: Option<PathBuf>,
    files: String,
    has_entry: bool,
    has_extra: bool,
    references: Vec<Vec<String>>,
    messages: Vec<Vec<String>>,
    status: String,
    can_finish: bool,
}

impl Shown {
    fn new(sheet: &ImportSheet, target: ImportTarget) -> Self {
        let (references, messages) = match &sheet.check {
            CheckState::Done(checked) => (
                checked.check.references.iter().map(reference_row).collect(),
                import_messages(checked),
            ),
            _ => (Vec::new(), Vec::new()),
        };
        let files = sheet
            .extra
            .iter()
            .map(|p| file_name(p))
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            name: sheet.name.clone(),
            location: sheet.parent.clone(),
            wsdl: sheet.entry.clone(),
            files: if files.is_empty() {
                "None".to_owned()
            } else {
                files
            },
            has_entry: sheet.entry.is_some(),
            has_extra: !sheet.extra.is_empty(),
            references,
            messages,
            status: import_status(sheet),
            can_finish: sheet.can_finish(target),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The form controls of the Servers tab.
#[derive(Debug)]
struct ServerForm {
    name: Retained<NSTextField>,
    url: Retained<NSTextField>,
    ignore_tls: Retained<NSButton>,
    auth_none: Retained<NSButton>,
    auth_basic: Retained<NSButton>,
    user: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    timeout: Retained<NSTextField>,
}

#[derive(Debug)]
pub struct SettingsIvars {
    key: ProjectKey,
    /// The model's servers as last shown.
    servers: RefCell<Vec<Server>>,
    suggested: RefCell<Vec<SuggestedServer>>,
    selected: Cell<Option<ServerId>>,
    window: OnceCell<Retained<NSWindow>>,
    tabs: OnceCell<Retained<NSTabView>>,
    table: OnceCell<Retained<TextTable>>,
    suggestions: OnceCell<(Retained<NSView>, Retained<TextTable>)>,
    form: OnceCell<ServerForm>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SettingsSheet` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsIvars]
    #[derive(Debug)]
    pub struct SettingsSheet;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SettingsSheet {}

    // SAFETY: `NSTextFieldDelegate` has no safety requirements.
    unsafe impl NSTextFieldDelegate for SettingsSheet {}

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for SettingsSheet {
        // A field is saved when editing ends, not on every keystroke: each save writes the
        // project and, for a password, the Keychain.
        // SAFETY: the signature matches `controlTextDidEndEditing:`.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn text_did_end_editing(&self, _notification: &NSNotification) {
            self.commit_form();
        }
    }

    impl SettingsSheet {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(formChanged:))]
        fn form_changed(&self, _sender: Option<&AnyObject>) {
            self.commit_form();
        }

        #[unsafe(method(addServer:))]
        fn add_server_action(&self, _sender: Option<&AnyObject>) {
            self.add_server();
        }

        #[unsafe(method(removeServer:))]
        fn remove_server_action(&self, _sender: Option<&AnyObject>) {
            self.remove_selected();
        }

        #[unsafe(method(confirmSuggestion:))]
        fn confirm_suggestion_action(&self, _sender: Option<&AnyObject>) {
            let row = self.suggestions().table().selectedRow();
            self.confirm_suggestion(usize::try_from(row).unwrap_or(0));
        }

        #[unsafe(method(done:))]
        fn done(&self, _sender: Option<&AnyObject>) {
            // Ending the field editor saves the field being edited.
            self.window().makeFirstResponder(None);
            self.commit_form();
            end_sheet(self.window());
        }
    }
);

impl SettingsSheet {
    pub fn new(key: ProjectKey, project: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SettingsIvars {
            key,
            servers: RefCell::new(Vec::new()),
            suggested: RefCell::new(Vec::new()),
            selected: Cell::new(None),
            window: OnceCell::new(),
            tabs: OnceCell::new(),
            table: OnceCell::new(),
            suggestions: OnceCell::new(),
            form: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = TextTable::new(&[], mtm);
        let weak = objc2::rc::Weak::from(&*this);
        table.on_click(move |row| {
            if let Some(sheet) = weak.load() {
                sheet.select(row);
            }
        });
        let suggested = TextTable::new(&["Port", "Address"], mtm);
        // A table has no height of its own: without one it collapsed, and the button drew over
        // its header.
        suggested
            .view()
            .heightAnchor()
            .constraintEqualToConstant(SUGGESTIONS_HEIGHT)
            .setActive(true);
        let suggestions = column_stack(
            vec![
                label("Suggested by the WSDL:", mtm),
                view(suggested.view().retain()),
                view(target_button(
                    "Add Server",
                    &this,
                    sel!(confirmSuggestion:),
                    mtm,
                )),
            ],
            mtm,
        );
        set_width(suggested.view(), LIST_WIDTH);
        let suggestions = view(suggestions);
        let form = this.server_form(mtm);
        let fields = grid(
            vec![
                vec![label("Name:", mtm), view(form.name.clone())],
                vec![label("URL:", mtm), view(form.url.clone())],
                vec![label("TLS:", mtm), view(form.ignore_tls.clone())],
                vec![
                    label("Auth:", mtm),
                    row(
                        vec![view(form.auth_none.clone()), view(form.auth_basic.clone())],
                        mtm,
                    ),
                ],
                vec![label("User:", mtm), view(form.user.clone())],
                vec![label("Password:", mtm), view(form.password.clone())],
                vec![
                    label("Timeout:", mtm),
                    row(vec![view(form.timeout.clone()), label("s", mtm)], mtm),
                ],
            ],
            mtm,
        );
        set_width(&form.timeout, TIMEOUT_WIDTH);
        let buttons = [
            square_button("+", &this, sel!(addServer:), mtm),
            square_button("−", &this, sel!(removeServer:), mtm),
        ];
        let buttons = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&buttons), mtm);
        buttons.setSpacing(0.0);
        let list = column_stack(
            vec![
                view(table.view().retain()),
                view(buttons),
                suggestions.clone(),
            ],
            mtm,
        );
        // The server list takes the height the suggestions leave.
        list.setDistribution(NSStackViewDistribution::Fill);
        // The +/− buttons sit right under the list, as in System Settings.
        list.setSpacing(0.0);
        list.setCustomSpacing_afterView(12.0, &list.arrangedSubviews().objectAtIndex(1));
        set_width(&list, LIST_WIDTH);
        // The form keeps its rows together at the top; a spacer below it takes the spare
        // height. Left to the grid, the spare height went to an arbitrary row and opened a
        // gap in the middle of the form.
        let spacer = NSView::new(mtm);
        spacer
            .setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
        let form_column = column_stack(vec![fields, spacer], mtm);
        form_column.setDistribution(NSStackViewDistribution::Fill);
        form_column.setAlignment(NSLayoutAttribute::Width);
        // List and form side by side; the form takes the remaining width.
        let servers = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(list.clone()), view(form_column.clone())]),
            mtm,
        );
        servers.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        servers.setAlignment(NSLayoutAttribute::Top);
        servers.setSpacing(20.0);
        servers.setEdgeInsets(NSEdgeInsets {
            top: 12.0,
            left: 12.0,
            bottom: 12.0,
            right: 12.0,
        });
        // The list and the form column run the full height.
        for v in [&*list, &*form_column] {
            v.heightAnchor()
                .constraintEqualToAnchor_constant(&servers.heightAnchor(), -24.0)
                .setActive(true);
        }

        let general = label(&format!("Name: {project}"), mtm);
        let tabs = NSTabView::new(mtm);
        for (title, content) in [("General", general), ("Servers", view(servers))] {
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(title));
            item.setView(Some(&content));
            tabs.addTabViewItem(&item);
        }
        tabs.selectTabViewItemAtIndex(1);

        let done = target_button("Done", &this, sel!(done:), mtm);
        done.setKeyEquivalent(ns_string!("\r"));
        let content = window_content(
            vec![view(tabs.clone())],
            trailing_row(vec![view(done)], mtm),
            mtm,
        );
        let window = sheet_window(
            &format!("{project} — Settings"),
            NSSize::new(640.0, 460.0),
            mtm,
        );
        window.setContentView(Some(&content));

        let _ = this.ivars().window.set(window);
        let _ = this.ivars().tabs.set(tabs);
        let _ = this.ivars().table.set(table);
        let _ = this.ivars().suggestions.set((suggestions, suggested));
        let _ = this.ivars().form.set(form);
        this.reload();
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    pub fn tabs(&self) -> &NSTabView {
        self.ivars().tabs.get().expect("set in new()")
    }

    pub fn table(&self) -> &TextTable {
        self.ivars().table.get().expect("set in new()")
    }

    /// The servers suggested by the WSDL's `soap:address`es, not yet confirmed.
    pub fn suggestions(&self) -> &TextTable {
        &self.ivars().suggestions.get().expect("set in new()").1
    }

    pub fn servers(&self) -> Vec<Server> {
        self.ivars().servers.borrow().clone()
    }

    pub fn selected(&self) -> Option<usize> {
        let id = self.ivars().selected.get()?;
        self.ivars()
            .servers
            .borrow()
            .iter()
            .position(|s| s.id == id)
    }

    pub fn name_field(&self) -> &NSTextField {
        &self.form().name
    }

    pub fn url_field(&self) -> &NSTextField {
        &self.form().url
    }

    pub fn user_field(&self) -> &NSTextField {
        &self.form().user
    }

    pub fn password_field(&self) -> &NSSecureTextField {
        &self.form().password
    }

    pub fn basic_auth_button(&self) -> &NSButton {
        &self.form().auth_basic
    }

    pub fn present(&self, parent: &NSWindow) {
        parent.beginSheet_completionHandler(self.window(), None);
    }

    /// Shows the model's servers and suggestions. The form keeps what it shows unless the
    /// selected server is gone, so a save does not disturb the field being edited.
    pub fn reload(&self) {
        let key = self.ivars().key;
        let (servers, suggested) = self
            .read(|app| {
                app.project(key)
                    .map(|w| (w.servers().to_vec(), w.suggested_servers().to_vec()))
            })
            .flatten()
            .unwrap_or_default();
        let (suggestions, suggested_table) = self.ivars().suggestions.get().expect("set in new()");
        suggestions.setHidden(suggested.is_empty());
        suggested_table.set_rows(
            suggested
                .iter()
                .map(|s| vec![s.port.clone(), s.url.clone()])
                .collect(),
        );
        *self.ivars().suggested.borrow_mut() = suggested;
        *self.ivars().servers.borrow_mut() = servers;
        self.reload_table();
        match self.selected() {
            Some(_) => {}
            None if self.ivars().servers.borrow().is_empty() => {
                self.ivars().selected.set(None);
                self.enable_form(false);
            }
            None => self.select(0),
        }
    }

    /// Loads server `row` into the form.
    pub fn select(&self, row: usize) {
        let Some(server) = self.ivars().servers.borrow().get(row).cloned() else {
            return;
        };
        self.ivars().selected.set(Some(server.id));
        let table = self.table().table();
        table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        let form = self.form();
        let (basic_auth, user) = match &server.auth {
            Auth::None => (false, ""),
            Auth::Basic { username } => (true, username.as_str()),
        };
        form.name.setStringValue(&NSString::from_str(&server.name));
        form.url.setStringValue(&NSString::from_str(&server.url));
        form.ignore_tls.setState(state(server.ignore_tls_errors));
        form.auth_none.setState(state(!basic_auth));
        form.auth_basic.setState(state(basic_auth));
        form.user.setStringValue(&NSString::from_str(user));
        form.password.setStringValue(ns_string!(""));
        form.timeout
            .setStringValue(&NSString::from_str(&server.timeout.as_secs().to_string()));
        self.enable_form(true);
        self.enable_auth_fields(basic_auth);
    }

    /// Saves the form into the selected server through the model; a typed password goes to
    /// the secret store and the field is emptied. Nothing is written if nothing changed.
    pub fn commit_form(&self) {
        let Some(current) = self
            .selected()
            .map(|row| self.ivars().servers.borrow()[row].clone())
        else {
            return;
        };
        let form = self.form();
        let basic_auth = form.auth_basic.state() == NSControlStateValueOn;
        self.enable_auth_fields(basic_auth);
        let mut server = current.clone();
        server.name = form.name.stringValue().to_string();
        server.url = form.url.stringValue().to_string();
        server.ignore_tls_errors = form.ignore_tls.state() == NSControlStateValueOn;
        server.auth = if basic_auth {
            Auth::Basic {
                username: form.user.stringValue().to_string(),
            }
        } else {
            Auth::None
        };
        // An unparsable or zero timeout keeps the stored one.
        if let Ok(secs) = form.timeout.stringValue().to_string().trim().parse::<u64>()
            && secs > 0
        {
            server.timeout = Duration::from_secs(secs);
        }
        let password = form.password.stringValue().to_string();
        let password = (basic_auth && !password.is_empty()).then_some(password);
        if server == current && password.is_none() {
            return;
        }
        let key = self.ivars().key;
        let saved = self
            .command("Could not save the server", |app| {
                app.update_server(key, &server, password.as_deref())
            })
            .is_some();
        if saved && password.is_some() {
            form.password.setStringValue(ns_string!(""));
        }
    }

    pub fn add_server(&self) {
        let key = self.ivars().key;
        if let Some(id) = self.command("Could not add a server", |app| app.add_server(key)) {
            self.select_id(id);
        }
    }

    pub fn remove_selected(&self) {
        let (Some(row), Some(id)) = (self.selected(), self.ivars().selected.get()) else {
            return;
        };
        let key = self.ivars().key;
        self.ivars().selected.set(None);
        self.command("Could not delete the server", |app| {
            app.delete_server(key, id)
        });
        let remaining = self.ivars().servers.borrow().len();
        if remaining > 0 {
            self.select(row.min(remaining - 1));
        }
    }

    /// Adds suggestion `row` as a server with its address as suggested, and selects it for
    /// editing.
    pub fn confirm_suggestion(&self, row: usize) {
        let Some(url) = self
            .ivars()
            .suggested
            .borrow()
            .get(row)
            .map(|s| s.url.clone())
        else {
            return;
        };
        let key = self.ivars().key;
        let before: Vec<ServerId> = self.ivars().servers.borrow().iter().map(|s| s.id).collect();
        self.command("Could not add the server", |app| {
            app.confirm_suggested_server(key, row, &url)
        });
        let added = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .map(|s| s.id)
            .find(|id| !before.contains(id));
        if let Some(id) = added {
            self.select_id(id);
        }
    }

    fn select_id(&self, id: ServerId) {
        let row = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .position(|s| s.id == id);
        if let Some(row) = row {
            self.select(row);
        }
    }

    fn form(&self) -> &ServerForm {
        self.ivars().form.get().expect("set in new()")
    }

    fn reload_table(&self) {
        let rows = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .map(|s| vec![s.name.clone()])
            .collect();
        self.table().set_rows(rows);
        if let Some(row) = self.selected() {
            self.table()
                .table()
                .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
        }
    }

    fn enable_form(&self, enabled: bool) {
        let form = self.form();
        for field in [&form.name, &form.url, &form.timeout] {
            field.setEnabled(enabled);
        }
        for button in [&form.ignore_tls, &form.auth_none, &form.auth_basic] {
            button.setEnabled(enabled);
        }
        if !enabled {
            self.enable_auth_fields(false);
        }
    }

    fn enable_auth_fields(&self, basic_auth: bool) {
        let form = self.form();
        form.user.setEnabled(basic_auth);
        form.password.setEnabled(basic_auth);
    }

    fn server_form(&self, mtm: MainThreadMarker) -> ServerForm {
        let text = |placeholder: &str| {
            let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            // SAFETY: this object owns the field through its window, so it outlives the
            // field's weak delegate reference.
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
            field
        };
        // SAFETY (buttons): this object owns them through its window, so it outlives their
        // weak target references.
        let (ignore_tls, auth_none, auth_basic) = unsafe {
            (
                NSButton::checkboxWithTitle_target_action(
                    ns_string!("Ignore certificate errors"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("None"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
                NSButton::radioButtonWithTitle_target_action(
                    ns_string!("Basic"),
                    Some(self),
                    Some(sel!(formChanged:)),
                    mtm,
                ),
            )
        };
        let password = NSSecureTextField::new(mtm);
        password.setPlaceholderString(Some(ns_string!("Saved to the Keychain")));
        // SAFETY: as for the text fields above.
        unsafe { password.setDelegate(Some(ProtocolObject::from_ref(self))) };
        ServerForm {
            name: text("Name"),
            url: text("https://"),
            ignore_tls,
            auth_none,
            auth_basic,
            user: text("User"),
            password,
            timeout: text("60"),
        }
    }
}

fn state(on: bool) -> NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

/// Ends `sheet` on whatever window it is attached to.
fn end_sheet(sheet: &NSWindow) {
    if let Some(parent) = sheet.sheetParent() {
        parent.endSheet(sheet);
    }
}

/// Ends a sheet, or hides a standalone window without closing it (closing means Cancel).
fn dismiss(window: &NSWindow) {
    if window.sheetParent().is_some() {
        end_sheet(window);
    } else {
        window.orderOut(None);
    }
}

fn sheet_window(title: &str, size: NSSize, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // SAFETY: the designated initializer, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the sheet's controller keeps the `Retained<NSWindow>`, so AppKit must not
    // release it on close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window
}

fn target_button(
    title: &str,
    target: &NSObject,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let target: &AnyObject = target;
    // SAFETY: every caller owns the button through its window, so the target outlives the
    // button's weak target reference.
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    view(NSTextField::labelWithString(&NSString::from_str(text), mtm))
}

fn view<T: Message + AsRef<NSView>>(v: Retained<T>) -> Retained<NSView> {
    let v: &NSView = (*v).as_ref();
    v.retain()
}

/// Dialog buttons, pushed to the trailing edge.
fn trailing_row(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    for v in &views {
        stack.addView_inGravity(v, NSStackViewGravity::Trailing);
    }
    view(stack)
}

fn row(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    view(stack)
}

/// Lets `v` take a form row's spare width and give it up first, so a long value is shortened
/// rather than widening the window or pushing its buttons out.
fn shrinkable(v: &NSView) {
    v.setContentHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultLow - 1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    v.setContentCompressionResistancePriority_forOrientation(
        NSLayoutPriorityDefaultLow - 1.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
}

/// A form value with its buttons: the value takes the spare width, the buttons keep theirs.
fn value_row(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack.setAlignment(NSLayoutAttribute::FirstBaseline);
    view(stack)
}

fn column_stack(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack
}

/// A window's content: a column with the standard 20 pt margin, its buttons at the trailing
/// edge. Every view spans the width inside the margins, and the column fills the window's
/// height (see `layout`): views without an intrinsic height, such as tables, take the slack.
fn window_content(
    mut views: Vec<Retained<NSView>>,
    buttons: Retained<NSView>,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    const MARGIN: f64 = 20.0;
    views.push(buttons);
    let stack = column_stack(views.clone(), mtm);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack.setEdgeInsets(layout::insets(MARGIN, MARGIN, MARGIN, MARGIN));
    stack.setSpacing(12.0);
    // Pinned rather than left to the stack's alignment, which let a long value widen its
    // row past the window's edges.
    for v in &views {
        v.widthAnchor()
            .constraintEqualToAnchor_constant(&stack.widthAnchor(), -2.0 * MARGIN)
            .setActive(true);
    }
    view(stack)
}

/// A form: labels right-aligned on the controls' baselines, controls filling the rest, as
/// in macOS settings panes.
fn grid(rows: Vec<Vec<Retained<NSView>>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let rows: Vec<Retained<NSArray<NSView>>> = rows
        .iter()
        .map(|r| NSArray::from_retained_slice(r))
        .collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
    grid.setColumnSpacing(8.0);
    grid.setRowSpacing(10.0);
    grid.columnAtIndex(0)
        .setXPlacement(NSGridCellPlacement::Trailing);
    // Labels keep their width so the controls' column takes the spare width; otherwise the
    // grid gives it to the labels and the form sits at the window's trailing edge.
    for label in rows.iter().filter_map(|r| r.firstObject()) {
        label.setContentHuggingPriority_forOrientation(
            NSLayoutPriorityDefaultHigh,
            NSLayoutConstraintOrientation::Horizontal,
        );
    }
    grid.columnAtIndex(1)
        .setXPlacement(NSGridCellPlacement::Fill);
    view(grid)
}

/// Pins `v`'s width, which a stack view respects where frames are ignored.
fn set_width(v: &NSView, width: f64) {
    v.widthAnchor()
        .constraintEqualToConstant(width)
        .setActive(true);
}

/// The small square +/− buttons under a list.
fn square_button(
    title: &str,
    target: &NSObject,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let button = target_button(title, target, action, mtm);
    button.setBezelStyle(NSBezelStyle::SmallSquare);
    set_width(&button, SQUARE_BUTTON);
    button
        .heightAnchor()
        .constraintEqualToConstant(SQUARE_BUTTON)
        .setActive(true);
    view(button)
}

impl ModelAccess for ImportSheetController {}

impl ModelAccess for SettingsSheet {}
