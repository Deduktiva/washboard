//! The welcome window, shown when no project window is open (PLAN §8, `docs/gui-draft.html`):
//! icon, name and version on the left, New/Open buttons, recent projects on the right.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSButton, NSColor, NSControl,
    NSControlTextEditingDelegate, NSEvent, NSFont, NSImageView, NSLayoutAttribute, NSLineBreakMode,
    NSMenu, NSMenuDelegate, NSMenuItem, NSResponder, NSScrollView, NSStackView, NSTableCellView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle,
    NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
    NSWindowTabbingMode, NSWindowTitleVisibility, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURL, ns_string,
};

use crate::app::with_delegate;
use crate::layout;
use crate::menu::menu_item;

/// A recent project as the welcome window and File ▸ Open Recent list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentProject {
    /// The folder's name. The project's own name would mean opening its database.
    pub name: String,
    /// Shown abbreviated with `~`, as Finder does.
    pub path: String,
}

impl RecentProject {
    pub fn new(folder: &Path) -> RecentProject {
        let name = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path = match home.as_deref().and_then(|h| folder.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => folder.display().to_string(),
        };
        RecentProject { name, path }
    }
}

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 400.0;
const LEFT: f64 = 250.0;
const ICON: f64 = 96.0;
const BUTTON_WIDTH: f64 = 160.0;

define_class!(
    // SAFETY:
    // - NSTableView has no subclassing requirements beyond its designated initializers, which
    //   we inherit.
    // - `RecentTableView` does not implement `Drop`.
    #[unsafe(super(NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    pub struct RecentTableView;

    impl RecentTableView {
        // SAFETY: the signature matches `keyDown:`.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let is_return = event
                .charactersIgnoringModifiers()
                .is_some_and(|c| matches!(c.to_string().as_str(), "\r" | "\u{3}"));
            if is_return && self.selectedRow() >= 0 {
                // Return opens the selected project, as a double-click does.
                // SAFETY: the target is the welcome controller, whose `openRecent:` takes the
                // sender.
                if let Some(target) = self.target() {
                    let _: () = unsafe { msg_send![&*target, openRecent: Some(self)] };
                }
            } else {
                // SAFETY: `keyDown:` takes the event.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }
    }
);

#[derive(Debug)]
pub struct WelcomeIvars {
    recent: RefCell<Vec<RecentProject>>,
    window: OnceCell<Retained<NSWindow>>,
    table: OnceCell<Retained<RecentTableView>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `WelcomeController` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = WelcomeIvars]
    #[derive(Debug)]
    pub struct WelcomeController;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for WelcomeController {}

    // SAFETY: `NSTableViewDataSource` has no safety requirements.
    unsafe impl NSTableViewDataSource for WelcomeController {
        // SAFETY: the signature matches `numberOfRowsInTableView:`.
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().recent.borrow().len() as NSInteger
        }
    }

    // SAFETY: `NSControlTextEditingDelegate` has no safety requirements.
    unsafe impl NSControlTextEditingDelegate for WelcomeController {}

    // SAFETY: `NSTableViewDelegate` has no safety requirements.
    unsafe impl NSTableViewDelegate for WelcomeController {
        // SAFETY: the signature matches `tableView:viewForTableColumn:row:`, and the returned
        // view is autoreleased by the `method_id` convention.
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let recent = self.ivars().recent.borrow();
            let project = usize::try_from(row).ok().and_then(|r| recent.get(r));
            project.map(|p| Retained::into_super(row_view(p, self.mtm())))
        }
    }

    // SAFETY: `NSMenuDelegate` has no safety requirements.
    unsafe impl NSMenuDelegate for WelcomeController {
        // SAFETY: the signature matches `menuNeedsUpdate:`.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.fill_context_menu(menu, self.table().clickedRow());
        }
    }

    impl WelcomeController {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(openRecent:))]
        fn open_recent_action(&self, sender: Option<&AnyObject>) {
            if let Some(row) = self.acted_on(sender) {
                self.open_recent(row);
            }
        }

        #[unsafe(method(showRecentInFinder:))]
        fn show_in_finder_action(&self, sender: Option<&AnyObject>) {
            if let Some(row) = self.acted_on(sender) {
                self.show_in_finder(row);
            }
        }

        #[unsafe(method(removeRecent:))]
        fn remove_recent_action(&self, sender: Option<&AnyObject>) {
            if let Some(row) = self.acted_on(sender) {
                self.remove_recent(row);
            }
        }
    }
);

impl WelcomeController {
    /// Opens the recent project in `row`; the double-click action of the table. Rows are in
    /// the model's order.
    pub fn open_recent(&self, row: usize) {
        crate::app::with_delegate(self.mtm(), |app| {
            app.open_recent(row);
        });
    }

    /// Shows the project folder in `row` in Finder.
    pub fn show_in_finder(&self, row: usize) {
        let Some(folder) = self.folder(row) else {
            return;
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&folder.to_string_lossy()));
        NSWorkspace::sharedWorkspace()
            .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
    }

    /// Takes the project in `row` off the list; the folder stays where it is.
    pub fn remove_recent(&self, row: usize) {
        with_delegate(self.mtm(), |d| {
            d.update(|app| app.remove_recent_project(row));
            d.sync();
        });
    }

    /// The context menu for `row`: Open, Show in Finder, Remove from List; none off the rows.
    pub fn fill_context_menu(&self, menu: &NSMenu, row: NSInteger) {
        menu.removeAllItems();
        if row < 0 || self.folder(row as usize).is_none() {
            return;
        }
        let items: [Option<(&str, Sel)>; 4] = [
            Some(("Open", sel!(openRecent:))),
            Some(("Show in Finder", sel!(showRecentInFinder:))),
            None,
            Some(("Remove from List", sel!(removeRecent:))),
        ];
        for entry in items {
            let Some((title, action)) = entry else {
                menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
                continue;
            };
            let item = menu_item(title, Some(action), "", self.mtm());
            // SAFETY: the target is this controller, which implements every action used here
            // and owns the window the menu belongs to.
            unsafe { item.setTarget(Some(self)) };
            item.setTag(row);
            menu.addItem(&item);
        }
    }

    /// The row an action is for: a context menu item's tag, else the clicked row (a
    /// double-click), else the selected row (Return).
    fn acted_on(&self, sender: Option<&AnyObject>) -> Option<usize> {
        if let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) {
            return usize::try_from(item.tag()).ok();
        }
        let table = self.table();
        let row = match table.clickedRow() {
            -1 => table.selectedRow(),
            row => row,
        };
        usize::try_from(row).ok()
    }

    /// The folder of the recent project in `row`, from the model.
    fn folder(&self, row: usize) -> Option<PathBuf> {
        with_delegate(self.mtm(), |d| {
            d.read(|app| app.recent_projects().get(row).cloned())
        })
        .flatten()
        .flatten()
    }

    /// The model's recent projects, most recent first. The first is selected, so Return opens
    /// the most recent project.
    pub fn set_recent(&self, recent: Vec<RecentProject>) {
        let any = !recent.is_empty();
        *self.ivars().recent.borrow_mut() = recent;
        let table = self.table();
        table.reloadData();
        if any && table.selectedRow() < 0 {
            table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        }
    }

    pub fn recent(&self) -> Vec<RecentProject> {
        self.ivars().recent.borrow().clone()
    }

    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WelcomeIvars {
            recent: RefCell::new(Vec::new()),
            window: OnceCell::new(),
            table: OnceCell::new(),
        });
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let window = window(mtm);
        let content = window
            .contentView()
            .expect("a new window has a content view");
        content.addSubview(&left_pane(mtm));
        let (scroll, table) = recent_table(mtm);
        // SAFETY: the controller owns the window, which owns the table, so it outlives the
        // table's weak references to its data source, delegate and target.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*this)));
            table.setTarget(Some(&this));
            table.setDoubleAction(Some(sel!(openRecent:)));
        }
        // The items depend on the clicked row, so the menu is filled as it opens.
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        // SAFETY: the table retains its menu; the menu's delegate is this controller, which
        // owns the window and with it the table.
        unsafe { table.setMenu(Some(&menu)) };
        window.setInitialFirstResponder(Some(&table));
        content.addSubview(&scroll);
        let _ = this.ivars().window.set(window);
        let _ = this.ivars().table.set(table);
        this
    }

    pub fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("set in new()")
    }

    pub fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("set in new()")
    }

    pub fn show(&self) {
        self.window().center();
        self.window().makeKeyAndOrderFront(None);
        // The recent projects take the keys: ↑/↓ choose, Return opens.
        self.window().makeFirstResponder(Some(self.table()));
    }
}

fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView;
    let window = layout::owned_window(
        ns_string!("Welcome to Washboard"),
        NSSize::new(WIDTH, HEIGHT),
        style,
        mtm,
    );
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    // Not a document window, so it never joins the project windows' tabs.
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window
}

/// Icon, name, version and the two buttons, centred in the left part of the window. Auto
/// Layout, not frames: a stack view sizes its arranged views itself, so a frame set on the
/// icon is ignored and the column ends up pinned to a corner.
fn left_pane(mtm: MainThreadMarker) -> Retained<NSView> {
    let pane = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(LEFT, HEIGHT)),
    );
    pane.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);

    let mut views: Vec<Retained<NSView>> = Vec::new();
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        let image = NSImageView::imageViewWithImage(&icon, mtm);
        image
            .widthAnchor()
            .constraintEqualToConstant(ICON)
            .setActive(true);
        image
            .heightAnchor()
            .constraintEqualToConstant(ICON)
            .setActive(true);
        views.push(layout::view(image));
    }
    let name = NSTextField::labelWithString(ns_string!("Washboard"), mtm);
    name.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
    views.push(layout::view(name));
    let version = NSTextField::labelWithString(
        &NSString::from_str(&format!("Version {}", env!("CARGO_PKG_VERSION"))),
        mtm,
    );
    version.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let version: Retained<NSView> = layout::view(version);
    views.push(version.clone());
    for (title, action) in [
        ("New Project…", sel!(newProject:)),
        ("Open Project…", sel!(openProject:)),
    ] {
        // SAFETY: no target: the action goes up the responder chain to the app delegate,
        // whose handlers take the sender as their only argument.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                None,
                Some(action),
                mtm,
            )
        };
        button
            .widthAnchor()
            .constraintEqualToConstant(BUTTON_WIDTH)
            .setActive(true);
        views.push(layout::view(button));
    }

    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::CenterX);
    stack.setSpacing(8.0);
    stack.setCustomSpacing_afterView(24.0, &version);
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    pane.addSubview(&stack);
    stack
        .centerXAnchor()
        .constraintEqualToAnchor(&pane.centerXAnchor())
        .setActive(true);
    stack
        .centerYAnchor()
        .constraintEqualToAnchor(&pane.centerYAnchor())
        .setActive(true);
    pane
}

fn recent_table(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<RecentTableView>) {
    let frame = NSRect::new(NSPoint::new(LEFT, 0.0), NSSize::new(WIDTH - LEFT, HEIGHT));
    // SAFETY: `init` is NSTableView's designated initializer for code-built views.
    let table: Retained<RecentTableView> = unsafe { msg_send![RecentTableView::alloc(mtm), init] };
    let column =
        NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("project"));
    column.setWidth(WIDTH - LEFT);
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setRowHeight(40.0);
    table.setStyle(NSTableViewStyle::SourceList);

    let scroll = layout::vertical_scroll(&table, mtm);
    scroll.setFrame(frame);
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    (scroll, table)
}

fn row_view(project: &RecentProject, mtm: MainThreadMarker) -> Retained<NSTableCellView> {
    let name = NSTextField::labelWithString(&NSString::from_str(&project.name), mtm);
    let path = layout::small_label(&project.path, mtm);
    path.setTextColor(Some(&NSColor::secondaryLabelColor()));
    layout::truncating(&name, NSLineBreakMode::ByTruncatingTail);
    // Long paths keep both ends: the folder's name is at the end.
    layout::truncating(&path, NSLineBreakMode::ByTruncatingMiddle);
    let views = [layout::view(name.clone()), layout::view(path)];
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(0.0);
    layout::cell(&stack, Some(&name), mtm)
}
