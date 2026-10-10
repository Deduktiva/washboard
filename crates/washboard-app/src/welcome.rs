//! The welcome window, shown when no project window is open (PLAN §8, `docs/gui-draft.html`):
//! icon, name and version on the left, New/Open buttons, recent projects on the right.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSButton, NSColor, NSControlTextEditingDelegate,
    NSFont, NSImageView, NSLayoutAttribute, NSLineBreakMode, NSScrollView, NSStackView,
    NSTableCellView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTableViewStyle, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowStyleMask, NSWindowTabbingMode, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSArray, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};

use crate::layout;

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

#[derive(Debug)]
pub struct WelcomeIvars {
    recent: RefCell<Vec<RecentProject>>,
    window: OnceCell<Retained<NSWindow>>,
    table: OnceCell<Retained<NSTableView>>,
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

    impl WelcomeController {
        // SAFETY: action methods take the sender and return nothing.
        #[unsafe(method(openRecent:))]
        fn open_recent_action(&self, _sender: Option<&AnyObject>) {
            let row = self.ivars().table.get().map(|t| t.clickedRow());
            if let Some(row) = row.and_then(|r| usize::try_from(r).ok()) {
                self.open_recent(row);
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

    /// The model's recent projects, most recent first.
    pub fn set_recent(&self, recent: Vec<RecentProject>) {
        *self.ivars().recent.borrow_mut() = recent;
        self.table().reloadData();
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
        views.push(Retained::into_super(Retained::into_super(image)));
    }
    let name = NSTextField::labelWithString(ns_string!("Washboard"), mtm);
    name.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
    views.push(Retained::into_super(Retained::into_super(name)));
    let version = NSTextField::labelWithString(
        &NSString::from_str(&format!("Version {}", env!("CARGO_PKG_VERSION"))),
        mtm,
    );
    version.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let version: Retained<NSView> = Retained::into_super(Retained::into_super(version));
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
        views.push(Retained::into_super(Retained::into_super(button)));
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

fn recent_table(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSTableView>) {
    let frame = NSRect::new(NSPoint::new(LEFT, 0.0), NSSize::new(WIDTH - LEFT, HEIGHT));
    let table = NSTableView::new(mtm);
    let column =
        NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("project"));
    column.setWidth(WIDTH - LEFT);
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setRowHeight(40.0);
    table.setStyle(NSTableViewStyle::SourceList);

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(true);
    // Shown only when the content does not fit, also with legacy (always-on) scrollers.
    scroll.setAutohidesScrollers(true);
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
    let views = [
        Retained::into_super(Retained::into_super(name.clone())),
        Retained::into_super(Retained::into_super(path)),
    ];
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(0.0);
    layout::cell(&stack, Some(&name), mtm)
}
