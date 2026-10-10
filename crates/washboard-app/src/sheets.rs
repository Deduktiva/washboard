//! The import sheet of PLAN §8 (New Project and Replace WSDL), bound to the model's
//! `ImportSheet`. The model runs the import check and stores everything; the sheet shows its
//! state and forwards input. Project settings are in the Settings window (`settings_window`).

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBorderType, NSButton, NSControlTextEditingDelegate, NSGridCellPlacement, NSGridRowAlignment,
    NSGridView, NSLayoutAttribute, NSLayoutConstraintOrientation, NSLayoutPriorityDefaultHigh,
    NSLayoutPriorityDefaultLow, NSLineBreakMode, NSModalResponse, NSModalResponseOK, NSOpenPanel,
    NSPathControl, NSPathStyle, NSStackView, NSStackViewDistribution, NSStackViewGravity,
    NSTextField, NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{
    NSArray, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString, NSURL, ns_string,
};
use washboard_ui_model::{App, CheckState, ImportSheet, ImportTarget};

use crate::app::{ModelAccess, with_delegate};
use crate::layout::{self, view};
use crate::table::TextTable;
use crate::text::{import_files, import_messages, import_status, reference_row};

/// The ✓/✗ columns: wide enough for the mark, so the reference text gets the room.
const MARK_WIDTH: f64 = 22.0;
/// The import sheet's tables: the references grow with the window, the findings do not.
const MIN_REFERENCES_HEIGHT: f64 = 120.0;
const MESSAGES_HEIGHT: f64 = 90.0;

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
            // It only shows the choice; focus goes to the Choose… button beside it.
            control.setRefusesFirstResponder(true);
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
        let choose_wsdl = button("Choose…", sel!(chooseWsdl:));
        rows.push(vec![
            label("WSDL:", mtm),
            value_row(vec![view(wsdl.clone()), view(choose_wsdl.clone())], mtm),
        ]);
        rows.push(vec![
            label("Other files:", mtm),
            value_row(
                vec![view(files.clone()), view(add.clone()), view(clear.clone())],
                mtm,
            ),
        ]);
        let form = grid(rows, mtm);

        let references = TextTable::new(&["", "In", "Reference", "Resolved to"], mtm);
        let messages = TextTable::new(&["", "Message"], mtm);
        // Messages name files and locations; cut short they say little.
        messages.wrap_column(1);
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
        // New Project starts with the name; Replace WSDL with choosing the WSDL.
        if new_project {
            window.setInitialFirstResponder(Some(&name));
        } else {
            window.setInitialFirstResponder(Some(&choose_wsdl));
        }

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

    /// One row per reference: mark, importing file, reference as written, resolved file.
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
    /// whose WSDL names server addresses opens with its Servers pane, where each is one Add as
    /// Server… away from being the first server.
    pub fn finish(&self) {
        if !self.finish_button().isEnabled() {
            return;
        }
        match self.target() {
            ImportTarget::NewProject => {
                // The import check knows the addresses; the new project's schema loads later.
                let addresses = self
                    .read(|app| {
                        app.import_sheet(ImportTarget::NewProject).is_some_and(|sheet| {
                            matches!(&sheet.check, CheckState::Done(c) if !c.addresses.is_empty())
                        })
                    })
                    .unwrap_or(false);
                let Some(key) = self.command("Could not create the project", App::create_project)
                else {
                    return;
                };
                if addresses {
                    with_delegate(self.mtm(), |d| {
                        if let Some(project) = d.project(key) {
                            project.show_settings_servers();
                        }
                    });
                }
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
        Self {
            name: sheet.name.clone(),
            location: sheet.parent.clone(),
            wsdl: sheet.entry.clone(),
            files: import_files(sheet),
            has_entry: sheet.entry.is_some(),
            has_extra: !sheet.extra.is_empty(),
            references,
            messages,
            status: import_status(sheet, target),
            can_finish: sheet.can_finish(target),
        }
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
    layout::owned_window(
        &NSString::from_str(title),
        size,
        NSWindowStyleMask::Titled,
        mtm,
    )
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

/// Dialog buttons, pushed to the trailing edge.
fn trailing_row(views: Vec<Retained<NSView>>, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    for v in &views {
        stack.addView_inGravity(v, NSStackViewGravity::Trailing);
    }
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

impl ModelAccess for ImportSheetController {}
