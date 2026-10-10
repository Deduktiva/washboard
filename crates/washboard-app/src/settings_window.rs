//! One Settings window for the app and every open project (Washboard ▸ Settings…, ⌘,), laid
//! out like System Settings: a sidebar of panes and grouped forms. It replaces the Project
//! Settings sheet and the small window WP-FORMAT-XML added.
//!
//! It is an ordinary window, not a sheet: it stays open beside the project windows and every
//! change applies as it is made, so there is no Done button. The sidebar has the app's section
//! ("Washboard"), whose settings live in the user defaults, and one section per open project,
//! whose settings live in the project's folder as before.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControlStateValue, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSImage, NSImageView, NSLayoutConstraintOrientation,
    NSLayoutPriorityDefaultHigh, NSLineBreakMode, NSOutlineView, NSOutlineViewDataSource,
    NSOutlineViewDelegate, NSPopUpButton, NSSplitViewController, NSSplitViewItem, NSStackView,
    NSSwitch, NSTableColumn, NSTableViewStyle, NSTextField, NSToolbar, NSToolbarDisplayMode,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWindowTabbingMode, NSWindowToolbarStyle, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSRect, NSSize,
    NSString, NSURL, ns_string,
};
use washboard_ui_model::{FormatSettings, INDENT_RANGE, ProjectKey};

use crate::app::{ModelAccess, with_delegate};
use crate::form;
use crate::layout::{self, view};
use crate::servers_pane::ServersPane;
use crate::sheets::ImportSheetController;
use crate::text::count;

/// Which pane the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// Washboard ▸ General: the app's settings.
    App,
    /// A project's name, folder and WSDL files.
    ProjectGeneral(ProjectKey),
    /// A project's servers.
    Servers(ProjectKey),
}

impl Pane {
    fn project(self) -> Option<ProjectKey> {
        match self {
            Pane::App => None,
            Pane::ProjectGeneral(key) | Pane::Servers(key) => Some(key),
        }
    }
}

/// The selected pane in the user defaults: `app`, or `general:` / `servers:` and the project's
/// folder, which, unlike its key, is the same on the next launch.
pub const PANE_KEY: &str = "SettingsPane";
const FRAME_NAME: &str = "Settings";

#[derive(Debug)]
pub struct ItemIvars {
    /// `None` for a section header.
    pane: Option<Pane>,
    title: String,
    symbol: &'static str,
    children: Vec<Retained<PaneItem>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `PaneItem` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ItemIvars]
    #[derive(Debug)]
    pub struct PaneItem;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for PaneItem {}
);

impl PaneItem {
    fn new(
        pane: Option<Pane>,
        title: &str,
        symbol: &'static str,
        children: Vec<Retained<PaneItem>>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ItemIvars {
            pane,
            title: title.to_owned(),
            symbol,
            children,
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn pane(&self) -> Option<Pane> {
        self.ivars().pane
    }

    pub fn title(&self) -> &str {
        &self.ivars().title
    }

    pub fn children(&self) -> &[Retained<PaneItem>] {
        &self.ivars().children
    }
}

/// The item behind an outline row. Every item the outline view hands back is one of ours.
fn item(object: &AnyObject) -> &PaneItem {
    object
        .downcast_ref::<PaneItem>()
        .expect("settings sidebar items are PaneItems")
}

#[derive(Debug, Default)]
pub struct SettingsWindowIvars {
    window: OnceCell<Retained<NSWindow>>,
    outline: OnceCell<Retained<NSOutlineView>>,
    /// Holds the selected pane's view.
    content: OnceCell<Retained<NSView>>,
    roots: RefCell<Vec<Retained<PaneItem>>>,
    selected: Cell<Option<Pane>>,
    /// Set while the controller changes the outline itself.
    applying: Cell<bool>,
    app_pane: OnceCell<Retained<NSView>>,
    indent: OnceCell<Retained<NSPopUpButton>>,
    on_save: OnceCell<Retained<NSSwitch>>,
    servers: RefCell<Vec<(ProjectKey, Retained<ServersPane>)>>,
    /// The shown General pane's list of WSDL files.
    wsdl_files: RefCell<Option<Retained<NSTextField>>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SettingsWindowController` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsWindowIvars]
    #[derive(Debug)]
    pub struct SettingsWindowController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for SettingsWindowController {}

    // SAFETY: `NSWindowDelegate` has no safety requirements.
    unsafe impl NSWindowDelegate for SettingsWindowController {
        // SAFETY: the signature matches `windowWillClose:`.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // Ending the field editor saves the field being edited.
            self.window().makeFirstResponder(None);
        }
    }

    // SAFETY: `NSOutlineViewDataSource` has no safety requirements.
    unsafe impl NSOutlineViewDataSource for SettingsWindowController {
        // SAFETY: the signature matches `outlineView:numberOfChildrenOfItem:`.
        #[unsafe(method(outlineView:numberOfChildrenOfItem:))]
        fn number_of_children(
            &self,
            _outline: &NSOutlineView,
            object: Option<&AnyObject>,
        ) -> NSInteger {
            match object {
                None => self.ivars().roots.borrow().len() as NSInteger,
                Some(object) => item(object).children().len() as NSInteger,
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
            object: Option<&AnyObject>,
        ) -> Retained<AnyObject> {
            let index = usize::try_from(index).expect("AppKit asks for a valid index");
            let child = match object {
                None => self.ivars().roots.borrow()[index].clone(),
                Some(object) => item(object).children()[index].clone(),
            };
            Retained::into_super(Retained::into_super(child))
        }

        // SAFETY: the signature matches `outlineView:isItemExpandable:`.
        #[unsafe(method(outlineView:isItemExpandable:))]
        fn is_expandable(&self, _outline: &NSOutlineView, object: &AnyObject) -> bool {
            !item(object).children().is_empty()
        }
    }

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements; a supertrait of
    // `NSOutlineViewDelegate`.
    unsafe impl NSControlTextEditingDelegate for SettingsWindowController {}

    // SAFETY: `NSOutlineViewDelegate` has no safety requirements.
    unsafe impl NSOutlineViewDelegate for SettingsWindowController {
        // SAFETY: the signature matches `outlineView:viewForTableColumn:item:`.
        #[unsafe(method_id(outlineView:viewForTableColumn:item:))]
        fn view_for_item(
            &self,
            _outline: &NSOutlineView,
            _column: Option<&NSTableColumn>,
            object: &AnyObject,
        ) -> Option<Retained<NSView>> {
            Some(self.row_view(item(object)))
        }

        // SAFETY: the signature matches `outlineView:isGroupItem:`.
        #[unsafe(method(outlineView:isGroupItem:))]
        fn is_group(&self, _outline: &NSOutlineView, object: &AnyObject) -> bool {
            item(object).pane().is_none()
        }

        // SAFETY: the signature matches `outlineView:shouldShowOutlineCellForItem:`.
        #[unsafe(method(outlineView:shouldShowOutlineCellForItem:))]
        fn shows_disclosure(&self, _outline: &NSOutlineView, _object: &AnyObject) -> bool {
            // Every section stays open, as in System Settings.
            false
        }

        // SAFETY: the signature matches `outlineView:shouldSelectItem:`.
        #[unsafe(method(outlineView:shouldSelectItem:))]
        fn should_select(&self, _outline: &NSOutlineView, object: &AnyObject) -> bool {
            item(object).pane().is_some()
        }

        // SAFETY: the signature matches `outlineViewSelectionDidChange:`.
        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            if self.ivars().applying.get() {
                return;
            }
            let pane = self
                .outline()
                .itemAtRow(self.outline().selectedRow())
                .and_then(|object| item(&object).pane());
            match pane {
                Some(pane) => self.select(pane),
                // Nothing selected: show the selection the window had.
                None => self.show_outline_selection(),
            }
        }
    }

    impl SettingsWindowController {
        // SAFETY: an action method: takes the sender, returns nothing.
        #[unsafe(method(settingChanged:))]
        fn setting_changed(&self, _sender: Option<&AnyObject>) {
            let settings = self.shown();
            with_delegate(self.mtm(), |d| d.set_format_settings(settings));
        }

        // SAFETY: an action method: takes the sender, returns nothing.
        #[unsafe(method(replaceWsdl:))]
        fn replace_wsdl_action(&self, _sender: Option<&AnyObject>) {
            if let Some(Pane::ProjectGeneral(key)) = self.selected() {
                self.replace_wsdl(key);
            }
        }

        // SAFETY: an action method: takes the sender, returns nothing.
        #[unsafe(method(showWsdlInFinder:))]
        fn show_wsdl_in_finder(&self, _sender: Option<&AnyObject>) {
            let Some(Pane::ProjectGeneral(key)) = self.selected() else {
                return;
            };
            let Some(entry) = self.wsdl_to_show(key) else {
                return;
            };
            let url = NSURL::fileURLWithPath(&NSString::from_str(&entry.to_string_lossy()));
            NSWorkspace::sharedWorkspace()
                .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
        }
    }
);

impl SettingsWindowController {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SettingsWindowIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let outline = NSOutlineView::new(mtm);
        let column =
            NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("pane"));
        outline.addTableColumn(&column);
        // SAFETY: `column` was just added to this outline view.
        unsafe { outline.setOutlineTableColumn(Some(&column)) };
        outline.setHeaderView(None);
        outline.setStyle(NSTableViewStyle::SourceList);
        outline.setFloatsGroupRows(false);
        // SAFETY: this controller owns the window and with it the outline view, so it
        // outlives the outline view's weak data source and delegate references.
        unsafe {
            outline.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            outline.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        }
        let sidebar_scroll = layout::vertical_scroll(&outline, mtm);
        sidebar_scroll.setDrawsBackground(false);

        let split = NSSplitViewController::new(mtm);
        let sidebar_vc = NSViewController::new(mtm);
        sidebar_vc.setView(&sidebar_scroll);
        // The sidebar behaviour gives it the system's (Liquid Glass) sidebar material.
        let sidebar = NSSplitViewItem::sidebarWithViewController(&sidebar_vc);
        sidebar.setMinimumThickness(190.0);
        sidebar.setMaximumThickness(260.0);
        sidebar.setCanCollapse(false);
        split.addSplitViewItem(&sidebar);
        let content = NSView::new(mtm);
        let content_vc = NSViewController::new(mtm);
        content_vc.setView(&content);
        split.addSplitViewItem(&NSSplitViewItem::splitViewItemWithViewController(
            &content_vc,
        ));

        let window = settings_window(mtm);
        window.setContentViewController(Some(&split));
        window.setContentSize(NSSize::new(720.0, 520.0));
        window.setContentMinSize(NSSize::new(620.0, 420.0));
        // The window holds its delegate weakly; this controller owns the window.
        window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        let name = NSString::from_str(FRAME_NAME);
        if !window.setFrameUsingName(&name) {
            window.center();
        }
        window.setFrameAutosaveName(&name);

        let (app_pane, indent, on_save) = app_pane(&this, mtm);
        let ivars = this.ivars();
        let _ = ivars.window.set(window);
        let _ = ivars.outline.set(outline);
        let _ = ivars.content.set(content);
        let _ = ivars.app_pane.set(app_pane);
        let _ = ivars.indent.set(indent);
        let _ = ivars.on_save.set(on_save);
        this.rebuild();
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    pub fn outline(&self) -> &NSOutlineView {
        self.ivars().outline.get().expect("set in new()")
    }

    /// Shows the window on `pane`; without one, on the pane last chosen if it still exists,
    /// else on the app's settings.
    pub fn show(&self, pane: Option<Pane>) {
        self.rebuild();
        let pane = pane
            .or_else(|| self.remembered())
            .filter(|p| self.exists(*p))
            .unwrap_or(Pane::App);
        self.select(pane);
        self.window().makeKeyAndOrderFront(None);
    }

    /// The shown pane's view.
    pub fn pane_view(&self) -> Option<Retained<NSView>> {
        self.ivars().content.get()?.subviews().firstObject()
    }

    /// The pane shown, once the window has shown one.
    pub fn selected(&self) -> Option<Pane> {
        self.ivars().selected.get()
    }

    /// Shows `pane` and remembers it for the next time the window opens.
    pub fn select(&self, pane: Pane) {
        if !self.exists(pane) {
            return;
        }
        // Ending the field editor saves the field being edited before its pane goes.
        self.window().makeFirstResponder(None);
        self.ivars().selected.set(Some(pane));
        let view = match pane {
            Pane::App => {
                self.set_shown(self.read(|app| app.format_settings()).unwrap_or_default());
                self.ivars().app_pane.get().expect("set in new()").clone()
            }
            Pane::ProjectGeneral(key) => self.project_general(key),
            Pane::Servers(key) => {
                let servers = self.servers(key);
                servers.reload();
                servers.view().clone()
            }
        };
        let content = self.ivars().content.get().expect("set in new()");
        for old in content.subviews().iter() {
            old.removeFromSuperview();
        }
        form::fill(content, &view);
        self.window()
            .setTitle(&NSString::from_str(&self.pane_title(pane)));
        self.show_outline_selection();
        self.remember(pane);
    }

    /// The open projects changed: one sidebar section per project. A closed project's panes
    /// go; if one of them was shown, the app's settings take its place.
    pub fn projects_changed(&self) {
        self.rebuild();
        let open: Vec<ProjectKey> = self
            .read(|app| app.projects().map(|(key, _)| key).collect())
            .unwrap_or_default();
        self.ivars()
            .servers
            .borrow_mut()
            .retain(|(key, _)| open.contains(key));
        match self.selected() {
            Some(pane) if !self.exists(pane) => self.select(Pane::App),
            Some(Pane::ProjectGeneral(key)) => self.select(Pane::ProjectGeneral(key)),
            _ => self.show_outline_selection(),
        }
    }

    /// The model's servers changed (`ServersChanged`).
    pub fn reload_servers(&self, key: ProjectKey) {
        let pane = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, p)| p.clone());
        if let Some(pane) = pane {
            pane.reload();
        }
    }

    /// The Servers pane of project `key`, created on first use.
    pub fn servers(&self, key: ProjectKey) -> Retained<ServersPane> {
        let existing = self
            .ivars()
            .servers
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, p)| p.clone());
        existing.unwrap_or_else(|| {
            let pane = ServersPane::new(key, &self.project_name(key), self.mtm());
            self.ivars().servers.borrow_mut().push((key, pane.clone()));
            pane
        })
    }

    /// The General pane's buttons under the WSDL files.
    fn wsdl_buttons(&self, mtm: MainThreadMarker) -> Vec<Retained<NSView>> {
        vec![
            view(target_button(
                "Show in Finder",
                self,
                sel!(showWsdlInFinder:),
                mtm,
            )),
            view(target_button(
                "Replace WSDL…",
                self,
                sel!(replaceWsdl:),
                mtm,
            )),
        ]
    }

    /// What the General pane's Show in Finder selects: project `key`'s entry WSDL, in the
    /// project's `wsdl/` folder with the files it imports.
    pub fn wsdl_to_show(&self, key: ProjectKey) -> Option<PathBuf> {
        self.read(|app| app.project(key)?.project().entry_wsdl().ok())
            .flatten()
    }

    /// The General pane's Replace WSDL…: the import sheet on this window, for project `key`.
    pub fn replace_wsdl(&self, key: ProjectKey) -> Option<Retained<ImportSheetController>> {
        let project = with_delegate(self.mtm(), |d| d.project(key)).flatten()?;
        project.show_replace_sheet(self.window())
    }

    /// The model replaced project `key`'s WSDL: its General pane lists the new files.
    pub fn wsdl_replaced(&self, key: ProjectKey) {
        if self.selected() == Some(Pane::ProjectGeneral(key)) {
            self.select(Pane::ProjectGeneral(key));
        }
    }

    /// The WSDL files a project's General pane lists, once one was shown.
    pub fn wsdl_files_shown(&self) -> Option<Vec<String>> {
        let label = self.ivars().wsdl_files.borrow().clone()?;
        Some(
            label
                .stringValue()
                .to_string()
                .lines()
                .map(str::to_owned)
                .collect(),
        )
    }

    /// The sidebar's rows, in order: section headers and panes.
    pub fn sidebar_titles(&self) -> Vec<String> {
        let outline = self.outline();
        (0..outline.numberOfRows())
            .filter_map(|row| outline.itemAtRow(row))
            .map(|object| item(&object).title().to_owned())
            .collect()
    }

    pub fn indent_popup(&self) -> &NSPopUpButton {
        self.ivars().indent.get().expect("set in new()")
    }

    pub fn on_save_switch(&self) -> &NSSwitch {
        self.ivars().on_save.get().expect("set in new()")
    }

    /// The app settings the controls show.
    pub fn shown(&self) -> FormatSettings {
        let index = usize::try_from(self.indent_popup().indexOfSelectedItem()).unwrap_or(1);
        FormatSettings {
            indent: INDENT_RANGE.start() + index,
            on_save: self.on_save_switch().state() == NSControlStateValueOn,
        }
    }

    fn set_shown(&self, settings: FormatSettings) {
        let index = settings.indent.saturating_sub(*INDENT_RANGE.start());
        self.indent_popup()
            .selectItemAtIndex(isize::try_from(index).unwrap_or(1));
        self.on_save_switch().setState(state(settings.on_save));
    }

    /// Whether `pane` belongs to the app or to a project that is open.
    fn exists(&self, pane: Pane) -> bool {
        match pane.project() {
            None => true,
            Some(key) => self.read(|app| app.project(key).is_some()).unwrap_or(false),
        }
    }

    fn project_name(&self, key: ProjectKey) -> String {
        self.read(|app| app.project(key).map(|w| w.name().to_owned()))
            .flatten()
            .unwrap_or_default()
    }

    /// The window's title: the pane, as System Settings titles its window.
    fn pane_title(&self, pane: Pane) -> String {
        match pane {
            Pane::App => "General".into(),
            Pane::ProjectGeneral(key) => format!("{} — General", self.project_name(key)),
            Pane::Servers(key) => format!("{} — Servers", self.project_name(key)),
        }
    }

    /// The sidebar from the model: the app's section, then one per open project.
    fn rebuild(&self) {
        let mtm = self.mtm();
        let projects: Vec<(ProjectKey, String)> = self
            .read(|app| {
                app.projects()
                    .map(|(key, w)| (key, w.name().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let mut roots = vec![PaneItem::new(
            None,
            "Washboard",
            "",
            vec![PaneItem::new(
                Some(Pane::App),
                "General",
                "gearshape",
                vec![],
                mtm,
            )],
            mtm,
        )];
        for (key, name) in projects {
            let panes = vec![
                PaneItem::new(
                    Some(Pane::ProjectGeneral(key)),
                    "General",
                    "info.circle",
                    vec![],
                    mtm,
                ),
                PaneItem::new(
                    Some(Pane::Servers(key)),
                    "Servers",
                    "server.rack",
                    vec![],
                    mtm,
                ),
            ];
            roots.push(PaneItem::new(None, &name, "folder", panes, mtm));
        }
        let old = std::mem::replace(&mut *self.ivars().roots.borrow_mut(), roots);
        let outline = self.outline();
        self.applying(|| {
            outline.reloadData();
            // SAFETY: nil expands every root item and, with `true`, all their descendants.
            unsafe { outline.expandItem_expandChildren(None, true) };
        });
        // The outline view no longer refers to the old items.
        drop(old);
    }

    /// Selects the shown pane's row.
    fn show_outline_selection(&self) {
        let outline = self.outline();
        let selected = self.selected();
        let row = (0..outline.numberOfRows()).find(|&row| {
            outline
                .itemAtRow(row)
                .is_some_and(|object| selected.is_some() && item(&object).pane() == selected)
        });
        self.applying(|| match row {
            Some(row) => {
                let rows = NSIndexSet::indexSetWithIndex(row as usize);
                outline.selectRowIndexes_byExtendingSelection(&rows, false);
            }
            // SAFETY: `deselectAll:` takes any sender.
            None => unsafe { outline.deselectAll(None) },
        });
    }

    fn applying(&self, f: impl FnOnce()) {
        let was = self.ivars().applying.replace(true);
        f();
        self.ivars().applying.set(was);
    }

    fn row_view(&self, item: &PaneItem) -> Retained<NSView> {
        let mtm = self.mtm();
        let title = NSTextField::labelWithString(&NSString::from_str(item.title()), mtm);
        layout::truncating(&title, NSLineBreakMode::ByTruncatingTail);
        let mut views = vec![];
        if let Some(image) = symbol(item.ivars().symbol) {
            let icon = NSImageView::imageViewWithImage(&image, mtm);
            if item.pane().is_none() {
                icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            }
            icon.setContentHuggingPriority_forOrientation(
                NSLayoutPriorityDefaultHigh,
                NSLayoutConstraintOrientation::Horizontal,
            );
            views.push(view(icon));
        }
        views.push(view(title.clone()));
        let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        stack.setSpacing(6.0);
        if item.pane().is_some() {
            title.setToolTip(Some(&NSString::from_str(item.title())));
        }
        view(layout::cell(&stack, Some(&title), mtm))
    }

    /// A project's name, folder and WSDL files, read when the pane is shown.
    fn project_general(&self, key: ProjectKey) -> Retained<NSView> {
        let mtm = self.mtm();
        let (name, folder, wsdl) = self
            .read(|app| {
                let window = app.project(key)?;
                let project = window.project();
                let dir = project.wsdl_dir();
                let entry = project.entry_wsdl().ok();
                let entry = entry.as_deref().and_then(|e| e.strip_prefix(&dir).ok());
                let files = wsdl_files(&dir, entry);
                Some((window.name().to_owned(), window.path().to_owned(), files))
            })
            .flatten()
            .unwrap_or_default();
        let folder_label = form::value_label(
            &folder.display().to_string(),
            NSLineBreakMode::ByTruncatingMiddle,
            mtm,
        );
        let files = form::value_lines(&wsdl.join("\n"), mtm);
        *self.ivars().wsdl_files.borrow_mut() = Some(files.clone());
        let name_label = form::value_label(&name, NSLineBreakMode::ByTruncatingTail, mtm);
        let group = form::group(
            vec![
                form::value_row("Name", &name_label, mtm),
                form::value_row("Folder", &folder_label, mtm),
                form::stacked_row("WSDL files", &files, mtm),
                form::button_row(self.wsdl_buttons(mtm), mtm),
            ],
            mtm,
        );
        form::page(
            vec![
                project_note(&name, mtm),
                form::Section::new(mtm)
                    .group(&group)
                    .text(
                        "Replace WSDL checks the new files as New Project does. Requests are \
                         kept as they are and validated against the new WSDL.",
                    )
                    .build(),
            ],
            mtm,
        )
    }

    fn remembered(&self) -> Option<Pane> {
        let value = with_delegate(self.mtm(), |d| {
            d.defaults().stringForKey(&NSString::from_str(PANE_KEY))
        })
        .flatten()?
        .to_string();
        if value == "app" {
            return Some(Pane::App);
        }
        let (kind, folder) = value.split_once(':')?;
        let key = self
            .read(|app| {
                app.projects()
                    .find(|(_, w)| w.path() == Path::new(folder))
                    .map(|(key, _)| key)
            })
            .flatten()?;
        match kind {
            "general" => Some(Pane::ProjectGeneral(key)),
            "servers" => Some(Pane::Servers(key)),
            _ => None,
        }
    }

    fn remember(&self, pane: Pane) {
        let folder = |key| {
            self.read(|app| app.project(key).map(|w| w.path().display().to_string()))
                .flatten()
        };
        let value = match pane {
            Pane::App => Some("app".to_owned()),
            Pane::ProjectGeneral(key) => folder(key).map(|f| format!("general:{f}")),
            Pane::Servers(key) => folder(key).map(|f| format!("servers:{f}")),
        };
        let Some(value) = value else { return };
        with_delegate(self.mtm(), |d| {
            let value = NSString::from_str(&value);
            // SAFETY: a string is a property list value, which the defaults store.
            unsafe {
                d.defaults()
                    .setObject_forKey(Some(&value), &NSString::from_str(PANE_KEY));
            }
        });
    }
}

impl ModelAccess for SettingsWindowController {}

/// The app's General pane: the format settings from WP-FORMAT-XML. Later app settings join it.
fn app_pane(
    target: &SettingsWindowController,
    mtm: MainThreadMarker,
) -> (
    Retained<NSView>,
    Retained<NSPopUpButton>,
    Retained<NSSwitch>,
) {
    let indent =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for width in INDENT_RANGE {
        indent.addItemWithTitle(&NSString::from_str(&count(width, "space")));
    }
    let on_save = NSSwitch::new(mtm);
    let target: &AnyObject = target;
    // SAFETY: the controller owns the controls through its window, so it outlives their weak
    // target references; `settingChanged:` takes the sender.
    unsafe {
        indent.setTarget(Some(target));
        indent.setAction(Some(sel!(settingChanged:)));
        on_save.setTarget(Some(target));
        on_save.setAction(Some(sel!(settingChanged:)));
    }
    let group = form::group(
        vec![
            form::control_row("Indent", &indent, mtm),
            form::control_row("Format the open request on Save All (⌘S)", &on_save, mtm),
        ],
        mtm,
    );
    let view = form::page(
        vec![
            form::Section::new(mtm)
                .header(&form::header("Formatting", mtm))
                .group(&group)
                .text(
                    "The indent is also used for new requests and responses. Autosave and Send \
                     never reformat a request. These settings apply to Washboard and every \
                     project.",
                )
                .build(),
        ],
        mtm,
    );
    (view, indent, on_save)
}

/// The WSDL files under `dir`, relative to it, the entry first.
fn wsdl_files(dir: &Path, entry: Option<&Path>) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            // Hidden names in `wsdl/` are Washboard's: `.previous` holds the set before the
            // last Replace WSDL.
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.display().to_string());
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    if let Some(entry) = entry.map(|e| e.display().to_string())
        && let Some(i) = files.iter().position(|f| *f == entry)
    {
        let first = files.remove(i);
        files.insert(0, first);
    }
    files
}

pub(crate) fn state(on: bool) -> NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn symbol(name: &str) -> Option<Retained<NSImage>> {
    if name.is_empty() {
        return None;
    }
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
}

/// Each project pane opens by saying whose settings these are and where they are kept, so
/// nobody takes them for the app's.
pub(crate) fn project_note(project: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    form::Section::new(mtm)
        .text(&format!(
            "These settings affect only the project “{project}”."
        ))
        .build()
}

pub(crate) fn target_button(
    title: &str,
    target: &NSObject,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let target: &AnyObject = target;
    // SAFETY: the settings window owns every button and its target, so the target outlives
    // the button's weak target reference.
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

fn settings_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    let window = layout::owned_window(
        ns_string!("Settings"),
        NSSize::new(720.0, 520.0),
        style,
        mtm,
    );
    // A unified, title-only toolbar, like System Settings: the title names the pane.
    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("Settings"));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window
}
