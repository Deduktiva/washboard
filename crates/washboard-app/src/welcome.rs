//! The welcome window, shown when no project window is open (PLAN §8, `docs/gui-draft.html`):
//! icon, name and version on the left, New/Open buttons, recent projects on the right.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSColor,
    NSControlTextEditingDelegate, NSFont, NSImageView, NSLayoutAttribute, NSScrollView,
    NSStackView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTableViewStyle, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSArray, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};

/// A recent project as the welcome window lists it.
#[derive(Debug, Clone)]
pub struct RecentProject {
    pub name: String,
    /// Shown abbreviated with `~`, as Finder does.
    pub path: String,
}

/// Placeholder rows until `washboard-ui-model` supplies the real list (`project::AppState`).
pub fn sample_recent_projects() -> Vec<RecentProject> {
    [
        ("Customer API", "~/Projects/soap/Customer API"),
        ("Billing", "~/Projects/soap/Billing"),
        ("Legacy ERP", "~/Work/erp-soap"),
    ]
    .into_iter()
    .map(|(name, path)| RecentProject {
        name: name.into(),
        path: path.into(),
    })
    .collect()
}

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 400.0;
const LEFT: f64 = 250.0;

#[derive(Debug)]
pub struct WelcomeIvars {
    recent: Vec<RecentProject>,
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
            self.ivars().recent.len() as NSInteger
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
            let project = usize::try_from(row)
                .ok()
                .and_then(|r| self.ivars().recent.get(r));
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
    /// Opens the recent project in `row`; the double-click action of the table.
    pub fn open_recent(&self, row: usize) {
        let Some(project) = self.ivars().recent.get(row) else {
            return;
        };
        crate::app::with_delegate(self.mtm(), |app| {
            app.open_project_window(&project.name);
        });
    }

    pub fn new(recent: Vec<RecentProject>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WelcomeIvars {
            recent,
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
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
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
    // SAFETY: the controller keeps the `Retained<NSWindow>`, so AppKit must not release it on
    // close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Welcome to Washboard"));
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    window
}

fn left_pane(mtm: MainThreadMarker) -> Retained<NSStackView> {
    let mut views: Vec<Retained<NSView>> = Vec::new();
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        let image = NSImageView::imageViewWithImage(&icon, mtm);
        image.setFrameSize(NSSize::new(96.0, 96.0));
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
    views.push(Retained::into_super(Retained::into_super(version)));
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
        views.push(Retained::into_super(Retained::into_super(button)));
    }

    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::CenterX);
    stack.setSpacing(10.0);
    stack.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(LEFT, HEIGHT),
    ));
    stack.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
    stack
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
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    (scroll, table)
}

fn row_view(project: &RecentProject, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let name = NSTextField::labelWithString(&NSString::from_str(&project.name), mtm);
    let path = NSTextField::labelWithString(&NSString::from_str(&project.path), mtm);
    path.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    path.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let views = [
        Retained::into_super(Retained::into_super(name)),
        Retained::into_super(Retained::into_super(path)),
    ];
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(0.0);
    stack
}
