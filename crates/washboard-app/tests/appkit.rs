//! Headless AppKit checks, run by the macOS CI job (WP-APP-SHELL and WP-APP-INTEGRATION in
//! `docs/TASKS.md`).
//!
//! `harness = false`: AppKit objects must be created on the main thread, and libtest runs
//! tests on worker threads. The app is launched once, through `NSApplication::run` until
//! `applicationDidFinishLaunching:` has been delivered; then each check inspects the real
//! objects from `washboard_app` and asserts on them. A failed assertion panics and fails the
//! run. Checks share the application and run in order.
//!
//! The app runs on a temp state dir and projects built from `fixtures/`, with an in-memory
//! secret store and dialogs that are recorded and answered by the checks.

#[cfg(target_os = "macos")]
type Check = (&'static str, fn(&checks::Ctx));

#[cfg(target_os = "macos")]
fn main() {
    let mtm = objc2::MainThreadMarker::new().expect("harness = false runs main on the main thread");
    let ctx = checks::Ctx::launch(mtm);
    let checks: &[Check] = &[
        ("lifecycle", checks::lifecycle),
        ("main_menu", checks::main_menu),
        ("welcome_window", checks::welcome_window),
        ("open_project_panel", checks::open_project_panel),
        ("project_window", checks::project_window),
        ("port_chips", checks::port_chips),
        ("sidebar_commands", checks::sidebar_commands),
        ("editor", checks::editor),
        ("editor_1mb_layout", checks::editor_1mb_layout),
        ("format_xml", checks::format_xml),
        ("dark_mode", checks::dark_mode),
        ("menu_validation", checks::menu_validation),
        ("diagnostics", checks::diagnostics),
        ("send", checks::send),
        ("http_log", checks::http_log),
        ("table_keys", checks::table_keys),
        ("completion_and_hover", checks::completion_and_hover),
        ("replace_wsdl", checks::replace_wsdl),
        ("new_project_sheet", checks::new_project_sheet),
        ("settings_window", checks::settings_window),
    ];
    for (name, check) in checks {
        print!("appkit check {name} ... ");
        check(&ctx);
        println!("ok");
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("appkit checks run on macOS only");
}

#[cfg(target_os = "macos")]
mod checks {
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{AllocAnyThread, MainThreadMarker, Message, msg_send};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSApplication, NSApplicationDidFinishLaunchingNotification, NSBox, NSColor, NSColorSpace,
        NSControlStateValueOn, NSEvent, NSEventModifierFlags, NSEventType,
        NSForegroundColorAttributeName, NSMenu, NSSplitViewItemBehavior, NSStackView,
        NSTableCellView, NSTextField, NSTextInputClient, NSTextView, NSToolbarDisplayMode, NSView,
        NSWindowOrderingMode, NSWindowTabbingMode, NSWritingToolsBehavior,
    };
    use objc2_foundation::{
        NSArray, NSDate, NSIndexSet, NSInteger, NSNotification, NSNotificationCenter,
        NSObjectProtocol, NSPoint, NSRange, NSRect, NSRunLoop, NSString, NSUserDefaults,
    };
    use tempfile::TempDir;
    use washboard_app::{
        AppDelegate, EditorController, ImportSheetController, NodeKind, Options,
        ProjectWindowController, SidebarNode, TextTable,
    };
    use washboard_app::{Pane, ServersPane};
    use washboard_core::model::{Auth, Server, ServerId};
    use washboard_core::project::{AppState, OpenProject, Project, WsdlFile, WsdlSet};
    use washboard_core::secrets::MemorySecretStore;
    use washboard_ui_model::{
        Alert, Confirm, DialogAnswer, DialogId, Dialogs, FormatSettings, ImportTarget, ProjectKey,
    };

    const DEFAULTS_SUITE: &str = "at.deduktiva.washboard.appkit-checks";

    /// How long launching may take before the run is abandoned instead of hanging CI.
    const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

    /// What the app asked the user, in order.
    #[derive(Debug, Default)]
    pub struct DialogLog {
        pub folders: Vec<DialogId>,
        pub alerts: Vec<Alert>,
        pub confirms: Vec<(DialogId, Confirm)>,
    }

    #[derive(Debug, Clone, Default)]
    struct RecordingDialogs(Rc<RefCell<DialogLog>>);

    impl Dialogs for RecordingDialogs {
        fn choose_project_folder(&self, id: DialogId) {
            self.0.borrow_mut().folders.push(id);
        }

        fn alert(&self, alert: Alert) {
            self.0.borrow_mut().alerts.push(alert);
        }

        fn confirm(&self, id: DialogId, confirm: Confirm) {
            self.0.borrow_mut().confirms.push((id, confirm));
        }
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    /// Creates a project named `name` in `<tmp>/<name>` from the legacy-rpc fixture.
    fn make_project(tmp: &TempDir, name: &str) -> PathBuf {
        let folder = tmp.path().join(name);
        let set = WsdlSet {
            files: vec![WsdlFile {
                source: fixtures().join("legacy-rpc/Legacy.wsdl"),
                dest: "Legacy.wsdl".into(),
            }],
            entry: "Legacy.wsdl".into(),
        };
        let mut project = Project::create(&folder, name, &set).expect("create project");
        for server in ["Staging", "Production"] {
            let server = Server {
                id: ServerId::new(),
                name: server.into(),
                url: format!("https://{}.invalid/legacy", server.to_lowercase()),
                ignore_tls_errors: false,
                auth: Auth::None,
                timeout: Duration::from_secs(5),
            };
            project.add_server(&server).expect("add server");
        }
        folder
    }

    /// Waits until the project's WSDL has loaded into the sidebar.
    fn wait_loaded(project: &ProjectWindowController) {
        wait_until("the WSDL to load", || {
            project.sidebar().roots()[1]
                .children()
                .first()
                .is_some_and(|n| n.kind() == NodeKind::Service)
        });
    }

    /// Every node under `roots`, depth first.
    fn all_nodes(roots: &[Retained<SidebarNode>]) -> Vec<Retained<SidebarNode>> {
        roots
            .iter()
            .flat_map(|n| {
                let mut nodes = vec![n.clone()];
                nodes.extend(all_nodes(n.children()));
                nodes
            })
            .collect()
    }

    /// Titles and enabled states of `row`'s context menu, as it would open.
    fn context_menu(project: &ProjectWindowController, row: NSInteger) -> Vec<(String, bool)> {
        let menu = NSMenu::new(MainThreadMarker::new().expect("main thread"));
        project.sidebar().fill_context_menu(&menu, row);
        menu.itemArray()
            .iter()
            .map(|i| (i.title().to_string(), i.isEnabled()))
            .collect()
    }

    /// Chooses `title` in `row`'s context menu, as a click on it would.
    fn context_click(ctx: &Ctx, project: &ProjectWindowController, row: NSInteger, title: &str) {
        let menu = NSMenu::new(MainThreadMarker::new().expect("main thread"));
        project.sidebar().fill_context_menu(&menu, row);
        let item = menu
            .itemArray()
            .iter()
            .find(|i| i.title().to_string() == title)
            .unwrap_or_else(|| panic!("{title:?} in the context menu of row {row}"));
        let action = item.action().expect("an action");
        // SAFETY: reading the item's target; the sidebar's actions take the sender.
        let sent = unsafe {
            ctx.app
                .sendAction_to_from(action, item.target().as_deref(), Some(&item))
        };
        assert!(sent, "{title:?} reached the sidebar");
    }

    fn request_names(project: &ProjectWindowController) -> Vec<String> {
        project.sidebar().roots()[0]
            .children()
            .iter()
            .map(|n| n.title())
            .collect()
    }

    /// The request files in the project folder, sorted.
    fn request_files(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder.join("requests"))
            .expect("requests dir")
            .filter_map(|e| {
                let path = e.ok()?.path();
                (path.extension()? == "xml")
                    .then(|| path.file_stem()?.to_str().map(String::from))?
            })
            .collect();
        names.sort();
        names
    }

    pub struct Ctx {
        pub app: Retained<NSApplication>,
        pub delegate: Retained<AppDelegate>,
        pub dialogs: Rc<RefCell<DialogLog>>,
        /// Restored on launch.
        pub project: PathBuf,
        /// Not open on launch.
        pub other: PathBuf,
        /// Holds the state dir and projects until the run ends.
        _tmp: TempDir,
    }

    impl Ctx {
        /// Opens the restored project through the model (focusing it if it is open) and
        /// returns its window controller.
        pub fn open(&self) -> Retained<ProjectWindowController> {
            let key = self
                .delegate
                .open_project_at(&self.project)
                .expect("the fixture project opens");
            self.delegate
                .project(key)
                .expect("a window for the project")
        }

        pub fn alerts(&self) -> Vec<Alert> {
            std::mem::take(&mut self.dialogs.borrow_mut().alerts)
        }

        /// `applicationDidFinishLaunching:` is sent from inside `run` (`finishLaunching` alone
        /// does not send it), so run the app and stop it once that notification has been
        /// delivered. The test's observer is added after the delegate's, so it runs second.
        pub fn launch(mtm: MainThreadMarker) -> Self {
            let tmp = TempDir::new().expect("temp dir");
            let project = make_project(&tmp, "Customer API");
            let other = make_project(&tmp, "Billing");
            let state_dir = tmp.path().join("state");
            AppState {
                open_projects: vec![
                    OpenProject::new(&project),
                    OpenProject::new(tmp.path().join("Gone")),
                ],
                recent_projects: vec![project.clone()],
            }
            .save(&state_dir)
            .expect("write state.json");

            let dialogs = RecordingDialogs::default();
            let log = dialogs.0.clone();
            // A scratch defaults domain, so the checks neither see nor change the user's
            // settings; one left over from an aborted run is cleared first.
            NSUserDefaults::standardUserDefaults()
                .removePersistentDomainForName(&NSString::from_str(DEFAULTS_SUITE));
            let options = Options {
                state_dir,
                secrets: Arc::new(MemorySecretStore::default()),
                dialogs: Some(Box::new(dialogs)),
                defaults_suite: Some(DEFAULTS_SUITE.into()),
            };
            let delegate = washboard_app::install(options, mtm);
            let app = NSApplication::sharedApplication(mtm);

            let stopper = app.clone();
            let on_launch = RcBlock::new(move |_: NonNull<NSNotification>| {
                stopper.stop(None);
                // `run` checks for `stop:` only after an event, so post one.
                let wake = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                    NSEventType::ApplicationDefined,
                    NSPoint::new(0.0, 0.0),
                    NSEventModifierFlags(0),
                    0.0,
                    0,
                    None,
                    0,
                    0,
                    0,
                );
                if let Some(wake) = wake {
                    stopper.postEvent_atStart(&wake, false);
                }
            });
            let center = NSNotificationCenter::defaultCenter();
            // SAFETY: no object filter and no queue: the block runs on the posting thread,
            // the main thread, where it may use `stopper`.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSApplicationDidFinishLaunchingNotification),
                    None,
                    None,
                    &on_launch,
                )
            };

            std::thread::spawn(|| {
                std::thread::sleep(LAUNCH_TIMEOUT);
                eprintln!("appkit: applicationDidFinishLaunching: not delivered, giving up");
                std::process::exit(1);
            });
            app.run();
            // SAFETY: `token` is the observer returned by `addObserverForName…` above.
            unsafe { center.removeObserver(token.as_ref()) };
            Self {
                app,
                delegate,
                dialogs: log,
                project,
                other,
                _tmp: tmp,
            }
        }
    }

    /// Launching restores the project from `state.json` and reports the missing one once;
    /// closing the last window does not quit.
    pub fn lifecycle(ctx: &Ctx) {
        let projects = ctx.delegate.projects();
        assert_eq!(projects.len(), 1, "the restored project");
        let window = projects[0].project_window();
        assert!(window.isVisible(), "restored window shown");
        assert_eq!(window.title().to_string(), "Customer API");
        assert!(
            !ctx.delegate.welcome().window().isVisible(),
            "no welcome window while a project is open"
        );
        let alerts = ctx.alerts();
        assert_eq!(alerts.len(), 1, "one alert for the missing project");
        assert!(alerts[0].message.contains("Gone"), "{alerts:?}");

        // Ask through the Objective-C runtime, as AppKit does, so this also checks the
        // selector is registered.
        // SAFETY: the selector takes an `NSApplication` and returns `BOOL`.
        let terminate: bool = unsafe {
            msg_send![&*ctx.delegate, applicationShouldTerminateAfterLastWindowClosed: &*ctx.app]
        };
        assert!(!terminate, "closing the last window must not quit");
    }

    const C: usize = NSEventModifierFlags::Command.0;
    const S: usize = NSEventModifierFlags::Shift.0;
    const O: usize = NSEventModifierFlags::Option.0;
    const CTRL: usize = NSEventModifierFlags::Control.0;

    /// (title, key equivalent, modifiers, action); `-` is a separator, an action of `>` a
    /// submenu.
    type Row = (&'static str, &'static str, usize, &'static str);

    const MENUS: &[(&str, &[Row])] = &[
        (
            "Washboard",
            &[
                ("About Washboard", "", 0, "orderFrontStandardAboutPanel:"),
                ("-", "", 0, ""),
                ("Settings…", ",", C, "showSettings:"),
                ("-", "", 0, ""),
                ("Services", "", 0, ">"),
                ("-", "", 0, ""),
                ("Hide Washboard", "h", C, "hide:"),
                ("Hide Others", "h", O | C, "hideOtherApplications:"),
                ("Show All", "", 0, "unhideAllApplications:"),
                ("-", "", 0, ""),
                ("Quit Washboard", "q", C, "terminate:"),
            ],
        ),
        (
            "File",
            &[
                ("New Project…", "n", S | C, "newProject:"),
                ("Open Project…", "o", C, "openProject:"),
                ("Open Recent", "", 0, ">"),
                ("-", "", 0, ""),
                ("Close", "w", C, "performClose:"),
                ("Save All", "s", C, "saveAll:"),
            ],
        ),
        (
            "Edit",
            &[
                ("Undo", "z", C, "undo:"),
                ("Redo", "z", S | C, "redo:"),
                ("-", "", 0, ""),
                ("Cut", "x", C, "cut:"),
                ("Copy", "c", C, "copy:"),
                ("Paste", "v", C, "paste:"),
                ("Select All", "a", C, "selectAll:"),
                ("-", "", 0, ""),
                ("Find", "", 0, ">"),
                ("-", "", 0, ""),
                ("Format XML", "i", CTRL, "formatXML:"),
            ],
        ),
        ("View", &[("Show Sidebar", "s", CTRL | C, "toggleSidebar:")]),
        (
            "Project",
            &[
                ("New Request", "n", C, "newRequest:"),
                ("Duplicate", "d", C, "duplicateRequest:"),
                ("Rename", "", 0, "renameRequest:"),
                ("Delete", "\u{8}", C, "deleteRequest:"),
                ("-", "", 0, ""),
                ("Validate", "b", C, "validateRequest:"),
                ("Send", "\r", C, "sendRequest:"),
                ("Cancel Send", ".", C, "cancelSend:"),
                ("-", "", 0, ""),
                ("Project Settings…", "", 0, "projectSettings:"),
            ],
        ),
        (
            "Window",
            &[
                ("Minimize", "m", C, "performMiniaturize:"),
                ("Zoom", "", 0, "performZoom:"),
                ("-", "", 0, ""),
                ("HTTP Log", "l", O | C, "showHttpLog:"),
                ("-", "", 0, ""),
                ("Bring All to Front", "", 0, "arrangeInFront:"),
            ],
        ),
        ("Help", &[]),
    ];

    fn rows(menu: &NSMenu) -> Vec<(String, String, usize, String)> {
        menu.itemArray()
            .iter()
            .map(|item| {
                if item.isSeparatorItem() {
                    return ("-".into(), String::new(), 0, String::new());
                }
                let action = if item.hasSubmenu() {
                    ">".into()
                } else {
                    item.action()
                        .map(|s| s.name().to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                let key = item.keyEquivalent().to_string();
                let modifiers = if key.is_empty() {
                    0
                } else {
                    item.keyEquivalentModifierMask().0
                };
                (item.title().to_string(), key, modifiers, action)
            })
            .collect()
    }

    /// Titles, shortcuts and actions of every menu item, and the menus AppKit manages.
    ///
    /// AppKit adds items of its own (Close All as an alternate of Close, Emoji & Symbols and
    /// AutoFill in Edit, window tiling in Window), varying by macOS release. Only our items
    /// are compared, in order; separators are not compared.
    pub fn main_menu(ctx: &Ctx) {
        let main = ctx.app.mainMenu().expect("a main menu");
        let items = main.itemArray();
        assert_eq!(items.len(), MENUS.len(), "top-level menus");
        for (item, (title, expected)) in items.iter().zip(MENUS) {
            let submenu = item.submenu().expect("top-level items have submenus");
            assert_eq!(submenu.title().to_string(), *title);
            let expected: Vec<_> = expected
                .iter()
                .filter(|(t, ..)| *t != "-")
                .map(|(t, k, m, a)| (t.to_string(), k.to_string(), *m, a.to_string()))
                .collect();
            let (ours, added): (Vec<_>, Vec<_>) = rows(&submenu)
                .into_iter()
                .filter(|(t, ..)| t != "-")
                .partition(|(t, ..)| expected.iter().any(|(e, ..)| e == t));
            assert_eq!(ours, expected, "{title} menu");
            if !added.is_empty() {
                let titles: Vec<_> = added.iter().map(|(t, ..)| t.as_str()).collect();
                print!("({title}: AppKit added {titles:?}) ");
            }
        }

        let windows = ctx
            .app
            .windowsMenu()
            .expect("the Window menu is registered");
        assert_eq!(windows.title().to_string(), "Window");
        let services = ctx
            .app
            .servicesMenu()
            .expect("the Services menu is registered");
        assert_eq!(services.title().to_string(), "Services");
        // Registered as the Help menu, it gets the system's menu search field.
        let help = ctx.app.helpMenu().expect("the Help menu is registered");
        assert_eq!(help.title().to_string(), "Help");
        let find = main
            .itemWithTitle(&NSString::from_str("Edit"))
            .and_then(|i| i.submenu())
            .and_then(|m| m.itemWithTitle(&NSString::from_str("Find")))
            .and_then(|i| i.submenu())
            .expect("Edit ▸ Find");
        let find_rows = rows(&find);
        let expected: Vec<(String, String, usize, String)> = [
            ("Find…", "f", C, "performFindPanelAction:"),
            ("Find and Replace…", "f", O | C, "performFindPanelAction:"),
            ("Find Next", "g", C, "performFindPanelAction:"),
            ("Find Previous", "g", S | C, "performFindPanelAction:"),
            ("Use Selection for Find", "e", C, "performFindPanelAction:"),
            ("Jump to Selection", "j", C, "centerSelectionInVisibleArea:"),
        ]
        .iter()
        .map(|(t, k, m, a)| (t.to_string(), k.to_string(), *m, a.to_string()))
        .collect();
        assert_eq!(find_rows, expected, "Find menu");
        let replace = find
            .itemWithTitle(&NSString::from_str("Find and Replace…"))
            .expect("Find and Replace");
        // `NSTextFinderActionShowReplaceInterface`.
        assert_eq!(replace.tag(), 12);

        // Settings is answered by the app delegate, so it is enabled with no window open.
        let app_menu = items
            .firstObject()
            .and_then(|i| i.submenu())
            .expect("app menu");
        let settings = app_menu.itemWithTitle(&objc2_foundation::NSString::from_str("Settings…"));
        app_menu.update();
        assert!(
            settings.expect("Settings item").isEnabled(),
            "Settings is enabled"
        );
    }

    fn open_recent_titles(ctx: &Ctx) -> Vec<String> {
        let file = ctx
            .app
            .mainMenu()
            .and_then(|m| m.itemWithTitle(&NSString::from_str("File")))
            .and_then(|i| i.submenu())
            .expect("File menu");
        let recent = file
            .itemWithTitle(&NSString::from_str("Open Recent"))
            .and_then(|i| i.submenu())
            .expect("Open Recent submenu");
        recent
            .itemArray()
            .iter()
            .filter(|i| !i.isSeparatorItem())
            .map(|i| i.title().to_string())
            .collect()
    }

    /// Closing the last project shows the welcome window listing it; opening it from the list
    /// or from File ▸ Open Recent brings it back, once.
    pub fn welcome_window(ctx: &Ctx) {
        let window = ctx.open().project_window();
        autoreleasepool(|_| window.performClose(None));
        assert!(!window.isVisible(), "project window closed");
        assert!(ctx.delegate.projects().is_empty(), "controller released");
        let welcome = ctx.delegate.welcome();
        assert!(welcome.window().isVisible(), "welcome window back");
        assert_eq!(
            welcome.window().tabbingMode(),
            NSWindowTabbingMode::Disallowed,
            "never a tab of a project window"
        );

        // The left column sits centred in its pane, clear of the edges.
        let content = welcome.window().contentView().expect("content view");
        content.layoutSubtreeIfNeeded();
        let pane = content.subviews().firstObject().expect("the left pane");
        let column = pane.subviews().firstObject().expect("the column");
        let (outer, inner) = (pane.bounds(), column.frame());
        assert!(
            inner.size.height > 0.0 && inner.size.width > 0.0,
            "{inner:?}"
        );
        let centre = |r: NSRect| {
            (
                r.origin.x + r.size.width / 2.0,
                r.origin.y + r.size.height / 2.0,
            )
        };
        let ((ox, oy), (ix, iy)) = (centre(outer), centre(inner));
        assert!(
            (ox - ix).abs() < 1.0 && (oy - iy).abs() < 1.0,
            "{outer:?} {inner:?}"
        );
        assert!(inner.origin.y > 0.0, "clear of the bottom edge: {inner:?}");

        let table = welcome.table();
        let scroll = table.enclosingScrollView().expect("the table scrolls");
        assert!(
            scroll.autohidesScrollers(),
            "no idle scroller beside one recent project"
        );
        assert_eq!(table.numberOfRows(), 1);
        let view = table
            .viewAtColumn_row_makeIfNecessary(0, 0, true)
            .expect("row 0 view");
        assert_fits(&view, "the recent project");
        let stack = view
            .subviews()
            .firstObject()
            .and_then(|v| v.downcast::<NSStackView>().ok())
            .expect("rows hold a stack of labels");
        let labels: Vec<String> = stack
            .arrangedSubviews()
            .iter()
            .map(|v: Retained<NSView>| {
                v.downcast::<NSTextField>()
                    .expect("labels")
                    .stringValue()
                    .to_string()
            })
            .collect();
        assert_eq!(labels[0], "Customer API");
        assert!(labels[1].ends_with("Customer API"), "{labels:?}");
        assert_eq!(open_recent_titles(ctx), ["Customer API", "Clear Menu"]);

        // A row's context menu; none off the rows.
        let menu = NSMenu::new(MainThreadMarker::new().expect("main thread"));
        welcome.fill_context_menu(&menu, 0);
        let titles: Vec<String> = menu
            .itemArray()
            .iter()
            .filter(|i| !i.isSeparatorItem())
            .map(|i| i.title().to_string())
            .collect();
        assert_eq!(titles, ["Open", "Show in Finder", "Remove from List"]);
        welcome.fill_context_menu(&menu, -1);
        assert_eq!(menu.numberOfItems(), 0, "no menu off the rows");

        // The most recent project is selected, and Return opens it.
        assert_eq!(
            table.selectedRow(),
            0,
            "the most recent project is selected"
        );
        let window = welcome.window();
        let key = |chars: &str| {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown,
                NSPoint::new(0.0, 0.0),
                NSEventModifierFlags::empty(),
                0.0,
                window.windowNumber(),
                None,
                &NSString::from_str(chars),
                &NSString::from_str(chars),
                false,
                36,
            )
            .expect("a key event")
        };
        table.keyDown(&key("\r"));
        assert_eq!(ctx.delegate.projects().len(), 1, "reopened by Return");
        assert!(!welcome.window().isVisible(), "welcome window hidden again");
        welcome.open_recent(0);
        assert_eq!(
            ctx.delegate.projects().len(),
            1,
            "focused, not opened twice"
        );
    }

    /// File ▸ Open Project… asks for a folder; Cancel does nothing, a project folder opens
    /// and goes to the top of Open Recent.
    pub fn open_project_panel(ctx: &Ctx) {
        let answer = |answer: DialogAnswer| {
            let id = ctx
                .dialogs
                .borrow_mut()
                .folders
                .pop()
                .expect("an open panel was requested");
            ctx.delegate.dialog_answered(id, answer);
        };
        // SAFETY: `openProject:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, openProject: None::<&AnyObject>] };
        answer(DialogAnswer::Cancelled);
        assert_eq!(ctx.delegate.projects().len(), 1);

        // SAFETY: as above.
        let _: () = unsafe { msg_send![&*ctx.delegate, openProject: None::<&AnyObject>] };
        answer(DialogAnswer::Folder(ctx.other.clone()));
        let projects = ctx.delegate.projects();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[1].name(), "Billing");
        assert_eq!(
            open_recent_titles(ctx),
            ["Billing", "Customer API", "Clear Menu"]
        );

        // Project windows group into one window's tabs; closing a tab closes its project only.
        let (first, second) = (projects[0].project_window(), projects[1].project_window());
        for w in [&first, &second] {
            assert_eq!(*w.tabbingIdentifier(), *washboard_app::project_tabbing_id());
            assert_eq!(w.tabbingMode(), NSWindowTabbingMode::Automatic);
        }
        first.addTabbedWindow_ordered(&second, NSWindowOrderingMode::Above);
        let tabs = first.tabbedWindows().map_or(0, |t| t.count());
        assert_eq!(tabs, 2, "merged into one window's tabs");
        autoreleasepool(|_| second.performClose(None));
        assert_eq!(ctx.delegate.projects().len(), 1);
        assert!(first.isVisible(), "the other tab stays open");
        assert!(ctx.alerts().is_empty());
    }

    /// The project window's toolbar, sidebar and split are built, and closing it brings the
    /// welcome window back.
    pub fn project_window(ctx: &Ctx) {
        let project = ctx.open();
        let window = project.project_window();
        assert!(window.isVisible(), "project window shown");
        assert!(
            !ctx.delegate.welcome().window().isVisible(),
            "welcome window hidden"
        );
        assert_eq!(window.title().to_string(), "Customer API");

        let toolbar = window.toolbar().expect("a toolbar");
        let ids: Vec<String> = toolbar
            .items()
            .iter()
            .map(|i| i.itemIdentifier().to_string())
            .collect();
        let expected: Vec<String> = washboard_app::toolbar_identifiers()
            .iter()
            .map(|i| i.to_string())
            .collect();
        assert_eq!(ids, expected, "toolbar items");
        assert_eq!(toolbar.displayMode(), NSToolbarDisplayMode::IconOnly);

        // Stacked views share the height instead of drawing over each other.
        window.layoutIfNeeded();
        let in_window = |v: &NSView| v.convertRect_toView(v.bounds(), None);
        let editor = in_window(project.editor().view());
        let issues = in_window(project.issues().view());
        let bar = in_window(project.request_bar().view());
        assert!(
            editor.size.height > 100.0,
            "the editor takes the slack: {editor:?}"
        );
        assert!(
            bar.origin.y >= editor.origin.y + editor.size.height - 0.5 && bar.size.height > 10.0,
            "the request bar sits above the editor: {bar:?} vs {editor:?}"
        );
        assert!(
            issues.origin.y + issues.size.height <= editor.origin.y + 0.5,
            "the issues bar sits below the editor: {issues:?} vs {editor:?}"
        );
        project.response().tabs().selectTabViewItemAtIndex(2);
        let history = || project.response().history().view().frame();
        // Give AppKit a few turns to frame the newly selected page.
        for _ in 0..40 {
            if history().size.height > 40.0 {
                break;
            }
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
        let history = history();
        // SAFETY: reading the view hierarchy on the main thread; nothing is changed.
        let page = unsafe { project.response().history().view().superview() };
        // SAFETY: as above.
        let container = page.as_ref().and_then(|p| unsafe { p.superview() });
        assert!(
            history.size.height > 40.0 && history.size.width > 400.0,
            "the history list has room: {history:?} in page {:?} in container {:?}, tab content \
             {:?}, tabs {:?}, pane {:?}",
            page.map(|v| v.frame()),
            container.map(|v| v.frame()),
            project.response().tabs().contentRect(),
            project.response().tabs().frame(),
            project.response().view().frame()
        );
        project.response().tabs().selectTabViewItemAtIndex(0);

        let split = project.split_view();
        let items = split.splitViewItems();
        assert_eq!(items.len(), 2, "sidebar and content");
        assert_eq!(
            items.firstObject().expect("sidebar item").behavior(),
            NSSplitViewItemBehavior::Sidebar
        );

        // Two groups, the requests, and every service, port and operation, all expanded.
        wait_loaded(&project);
        let rows = all_nodes(&project.sidebar().roots()).len();
        let outline = project.sidebar().outline().expect("outline built");
        assert_eq!(outline.numberOfRows() as usize, rows, "sidebar rows");
        assert!(
            !outline.floatsGroupRows(),
            "headers do not float, so the first one draws no separator"
        );

        // AppKit holds data sources and delegates weakly: they must survive a pool drain.
        autoreleasepool(|_| outline.reloadData());
        // SAFETY: reading the weak properties; nothing is called on the results.
        let (data_source, delegate) = unsafe { (outline.dataSource(), outline.delegate()) };
        assert!(data_source.is_some(), "outline data source alive");
        assert!(delegate.is_some(), "outline delegate alive");
        assert_eq!(
            outline.numberOfRows() as usize,
            rows,
            "sidebar rows after reload"
        );

        // Context menus: New Request on the REQUESTS header and on operations (disabled on an
        // unsupported one), nothing on the OPERATIONS header, services and ports.
        let row_of = |f: &dyn Fn(&SidebarNode) -> bool| {
            (0..outline.numberOfRows())
                .find(|&row| {
                    outline
                        .itemAtRow(row)
                        .is_some_and(|i| i.downcast_ref::<SidebarNode>().is_some_and(f))
                })
                .expect("a matching row")
        };
        let requests_header = row_of(&|n| n.title() == "REQUESTS");
        assert_eq!(
            context_menu(&project, requests_header),
            [("New Request".to_string(), true)]
        );
        let ops_header = row_of(&|n| n.title() == "OPERATIONS");
        assert!(context_menu(&project, ops_header).is_empty());
        let service = row_of(&|n| n.kind() == NodeKind::Service);
        assert!(context_menu(&project, service).is_empty());
        let port = row_of(&|n| n.kind() == NodeKind::Port);
        assert!(context_menu(&project, port).is_empty());
        let supported = row_of(&|n| n.kind() == NodeKind::Operation && n.unsupported().is_none());
        assert_eq!(
            context_menu(&project, supported),
            [("New Request".to_string(), true)]
        );
        let unsupported = row_of(&|n| n.unsupported().is_some());
        assert_eq!(
            context_menu(&project, unsupported),
            [("New Request".to_string(), false)]
        );
        let menu = outline.menu().expect("the outline has a context menu");
        // AppKit holds the menu's delegate weakly, too.
        assert!(menu.delegate().is_some(), "menu delegate alive");
        let pane = project
            .split_view()
            .splitViewItems()
            .firstObject()
            .expect("sidebar item");
        assert!(
            pane.viewController(MainThreadMarker::new().expect("main thread"))
                .view()
                .downcast::<objc2_app_kit::NSScrollView>()
                .is_ok(),
            "no buttons under the sidebar, only the list"
        );

        // Operations are listed, unsupported ones too, and can be selected.
        let nodes = all_nodes(&project.sidebar().roots());
        let chips: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind() == NodeKind::Port)
            .map(|n| (n.title(), n.chip()))
            .collect();
        assert_eq!(
            chips,
            [
                ("LegacyPort".to_owned(), Some("1.1")),
                ("LegacyEncodedPort".to_owned(), Some("1.1")),
            ]
        );
        assert!(
            nodes.iter().any(|n| n.unsupported().is_some()),
            "the rpc/encoded operation is shown"
        );
        let operation_row = (0..outline.numberOfRows())
            .find(|&row| {
                outline.itemAtRow(row).is_some_and(|item| {
                    item.downcast_ref::<SidebarNode>()
                        .is_some_and(|n| n.kind() == NodeKind::Operation)
                })
            })
            .expect("an operation row") as usize;
        outline.selectRowIndexes_byExtendingSelection(
            &NSIndexSet::indexSetWithIndex(operation_row),
            false,
        );
        assert_eq!(outline.selectedRow() as usize, operation_row);

        autoreleasepool(|_| window.performClose(None));
        assert!(!window.isVisible(), "project window closed");
        assert!(ctx.delegate.projects().is_empty(), "controller released");
        assert!(
            ctx.delegate.welcome().window().isVisible(),
            "welcome window back"
        );
    }

    /// A project with a SOAP 1.2 port marks it "1.2 · unsupported" beside the 1.1 port, and the
    /// chips fit their rows.
    pub fn port_chips(ctx: &Ctx) {
        let tmp = TempDir::new().expect("temp dir");
        let from = fixtures().join("customer");
        let set = WsdlSet {
            files: CUSTOMER_FILES
                .iter()
                .map(|f| WsdlFile {
                    source: from.join(f),
                    dest: (*f).into(),
                })
                .collect(),
            entry: "CustomerService.wsdl".into(),
        };
        let folder = tmp.path().join("Customers");
        Project::create(&folder, "Customers", &set).expect("create project");
        let key = ctx
            .delegate
            .open_project_at(&folder)
            .expect("the customer project opens");
        let project = ctx.delegate.project(key).expect("a window for the project");
        wait_loaded(&project);

        let outline = project.sidebar().outline().expect("outline built").retain();
        let ports: Vec<(NSInteger, Retained<SidebarNode>)> = (0..outline.numberOfRows())
            .filter_map(|row| {
                let item = outline.itemAtRow(row)?;
                let node = item.downcast::<SidebarNode>().ok()?;
                (node.kind() == NodeKind::Port).then_some((row, node))
            })
            .collect();
        let chips: Vec<_> = ports.iter().map(|(_, n)| (n.title(), n.chip())).collect();
        assert_eq!(
            chips,
            [
                ("CustomerPort".to_owned(), Some("1.1")),
                ("CustomerPort12".to_owned(), Some("1.2 · unsupported")),
            ]
        );
        project.project_window().layoutIfNeeded();
        for (row, node) in &ports {
            let cell = outline
                .viewAtColumn_row_makeIfNecessary(0, *row, true)
                .expect("a cell");
            assert_fits(&cell, &node.title());
            cell.layoutSubtreeIfNeeded();
            // The chip is the box beside the name, inside the cell and as tall as a line.
            let stack = cell.subviews().firstObject().expect("the cell has content");
            let chip = stack
                .subviews()
                .iter()
                .find(|v| v.downcast_ref::<NSBox>().is_some())
                .expect("a chip");
            let frame = chip.convertRect_toView(chip.bounds(), Some(&cell));
            let outer = cell.bounds();
            assert!(
                frame.size.width > 15.0
                    && frame.size.height > 10.0
                    && frame.origin.x + frame.size.width <= outer.size.width + 0.5,
                "{}'s chip fits: {frame:?} in {outer:?}",
                node.title()
            );
        }
        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// New, rename, duplicate and delete through the window's actions change the rows and the
    /// folder; the server popup follows the selected request.
    pub fn sidebar_commands(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        let outline = project.sidebar().outline().expect("outline built").retain();
        let window = project.project_window();
        let send = |action: &str| {
            let action = std::ffi::CString::new(action).expect("a selector name");
            // SAFETY: the window controller's actions take the sender.
            let sent = unsafe {
                ctx.app
                    .sendAction_to_from(Sel::register(&action), Some(&project), None)
            };
            assert!(sent, "{action:?} reached the window controller");
        };
        let selected_row = || {
            let id = project.selected_request();
            (0..outline.numberOfRows()).find(|&row| {
                outline.itemAtRow(row).is_some_and(|item| {
                    item.downcast_ref::<SidebarNode>()
                        .is_some_and(|n| n.request().is_some() && n.request() == id)
                })
            })
        };

        let popup = project.server_popup();
        assert!(popup.isEnabled());
        let titles: Vec<String> = popup.itemTitles().iter().map(|t| t.to_string()).collect();
        assert_eq!(titles, ["Staging", "Production"]);
        // A long server name widens the picker only so far; the title truncates.
        popup.addItemWithTitle(&NSString::from_str(
            "Production cluster behind the second load balancer in Frankfurt",
        ));
        popup.selectItemAtIndex(2);
        window
            .contentView()
            .expect("content")
            .layoutSubtreeIfNeeded();
        assert!(
            popup.frame().size.width <= 220.5,
            "the server popup is capped: {:?}",
            popup.frame()
        );
        project.reload_servers();
        let titles: Vec<String> = popup.itemTitles().iter().map(|t| t.to_string()).collect();
        assert_eq!(
            titles,
            ["Staging", "Production"],
            "reload restores the list"
        );

        send("newRequest:");
        assert_eq!(request_names(&project), ["Lookup 1"]);
        assert_eq!(request_files(&ctx.project), ["Lookup 1"]);
        assert!(
            project.selected_request().is_some(),
            "the new request is selected"
        );
        assert_eq!(selected_row(), Some(outline.selectedRow()), "and its row");
        // End the inline rename that New Request starts, without renaming.
        window.makeFirstResponder(Some(&outline));
        assert_eq!(request_names(&project), ["Lookup 1"]);

        // Inline rename: what the field holds when editing ends.
        let rename = |name: &str| {
            let row = selected_row().expect("a selected request");
            let view = outline
                .viewAtColumn_row_makeIfNecessary(0, row, true)
                .expect("row view");
            let field = view
                .downcast::<NSTableCellView>()
                .ok()
                // SAFETY: the sidebar's cells keep their text field alive for the row's lifetime.
                .and_then(|cell| unsafe { cell.textField() })
                .expect("name field");
            field.setStringValue(&NSString::from_str(name));
            // SAFETY: the notification's object is the text field, as AppKit sends it.
            let notification = unsafe {
                NSNotification::notificationWithName_object(
                    &NSString::from_str("NSControlTextDidEndEditingNotification"),
                    Some(&field),
                )
            };
            let sidebar = project.sidebar();
            // SAFETY: the text field delegate method takes the notification.
            let _: () = unsafe { msg_send![sidebar, controlTextDidEndEditing: &*notification] };
        };
        rename("Find customer");
        assert_eq!(request_names(&project), ["Find customer"]);
        assert_eq!(request_files(&ctx.project), ["Find customer"]);
        // A long name is shortened inside its row, not run past the sidebar.
        rename("Find customer by every field we have ever stored about them, twice over");
        let row = selected_row().expect("still selected");
        let cell = outline
            .viewAtColumn_row_makeIfNecessary(0, row, true)
            .expect("row view");
        assert_fits(&cell, "a long request name");
        rename("Find customer");
        rename("");
        assert_eq!(request_names(&project), ["Find customer"]);
        assert_eq!(ctx.alerts().len(), 1, "a refused name is an alert");

        // The popup remembers the server per request.
        popup.selectItemAtIndex(1);
        send("chooseServer:");
        assert_eq!(popup.indexOfSelectedItem(), 1);
        send("duplicateRequest:");
        assert_eq!(
            request_names(&project),
            ["Find customer", "Find customer copy"]
        );
        assert_eq!(
            request_files(&ctx.project),
            ["Find customer", "Find customer copy"]
        );
        assert_eq!(
            selected_row(),
            Some(outline.selectedRow()),
            "the copy is selected"
        );
        popup.selectItemAtIndex(0);
        send("chooseServer:");
        let original = NSIndexSet::indexSetWithIndex(1);
        outline.selectRowIndexes_byExtendingSelection(&original, false);
        assert_eq!(
            selected_row(),
            Some(1),
            "selecting a row selects its request"
        );
        assert_eq!(popup.indexOfSelectedItem(), 1, "the original's server");

        send("deleteRequest:");
        let (id, confirm) = ctx
            .dialogs
            .borrow_mut()
            .confirms
            .pop()
            .expect("delete asks first");
        assert!(confirm.title.contains("Find customer"), "{confirm:?}");
        ctx.delegate.dialog_answered(id, DialogAnswer::Confirmed);
        assert_eq!(request_names(&project), ["Find customer copy"]);
        assert_eq!(request_files(&ctx.project), ["Find customer copy"]);
        assert_eq!(
            selected_row(),
            Some(outline.selectedRow()),
            "the next one is selected"
        );
        assert_eq!(popup.indexOfSelectedItem(), 0, "the copy's server");
        assert!(ctx.alerts().is_empty());

        // The context menu acts on the clicked row. New Request on an operation puts the
        // request in its place by name and selects it.
        let operation_row = |name: &str| {
            (0..outline.numberOfRows())
                .find(|&row| {
                    outline.itemAtRow(row).is_some_and(|item| {
                        item.downcast_ref::<SidebarNode>().is_some_and(|n| {
                            n.kind() == NodeKind::Operation
                                && n.unsupported().is_none()
                                && n.title() == name
                        })
                    })
                })
                .expect("the operation's row")
        };
        let request_row = |name: &str| {
            (0..outline.numberOfRows())
                .find(|&row| {
                    outline.itemAtRow(row).is_some_and(|item| {
                        item.downcast_ref::<SidebarNode>()
                            .is_some_and(|n| n.kind() == NodeKind::Request && n.title() == name)
                    })
                })
                .expect("the request's row")
        };
        context_click(ctx, &project, operation_row("Lookup"), "New Request");
        window.makeFirstResponder(Some(&outline));
        assert_eq!(request_names(&project), ["Find customer copy", "Lookup 1"]);
        assert_eq!(selected_row(), Some(outline.selectedRow()));
        assert_eq!(selected_row(), Some(request_row("Lookup 1")));
        assert_eq!(
            context_menu(&project, request_row("Lookup 1")),
            [
                ("Rename".to_string(), true),
                ("Duplicate".to_string(), true),
                ("Validate".to_string(), true),
                ("Delete".to_string(), true),
            ]
        );

        // Renaming moves the request to its place; it stays selected.
        rename("A lookup");
        assert_eq!(request_names(&project), ["A lookup", "Find customer copy"]);
        assert_eq!(selected_row(), Some(request_row("A lookup")));
        assert_eq!(selected_row(), Some(outline.selectedRow()));

        // Duplicate and Delete on a row that is not selected.
        context_click(
            ctx,
            &project,
            request_row("Find customer copy"),
            "Duplicate",
        );
        assert_eq!(
            request_names(&project),
            ["A lookup", "Find customer copy", "Find customer copy copy"]
        );
        assert_eq!(selected_row(), Some(request_row("Find customer copy copy")));
        context_click(ctx, &project, request_row("A lookup"), "Delete");
        let (id, confirm) = ctx
            .dialogs
            .borrow_mut()
            .confirms
            .pop()
            .expect("delete asks first");
        assert!(confirm.title.contains("A lookup"), "{confirm:?}");
        ctx.delegate.dialog_answered(id, DialogAnswer::Confirmed);
        assert_eq!(
            request_names(&project),
            ["Find customer copy", "Find customer copy copy"]
        );
        assert_eq!(
            selected_row(),
            Some(request_row("Find customer copy copy")),
            "deleting another row keeps the selection"
        );

        // Validate and Rename select the clicked request first.
        context_click(ctx, &project, request_row("Find customer copy"), "Validate");
        assert_eq!(selected_row(), Some(request_row("Find customer copy")));
        context_click(
            ctx,
            &project,
            request_row("Find customer copy copy"),
            "Rename",
        );
        assert_eq!(selected_row(), Some(request_row("Find customer copy copy")));
        assert!(project.sidebar().is_editing(), "inline rename started");
        window.makeFirstResponder(Some(&outline));
        assert!(ctx.alerts().is_empty(), "{:?}", ctx.alerts());
    }

    fn temporary_color(editor: &EditorController, index: usize) -> Option<Retained<AnyObject>> {
        // SAFETY: an immutable AppKit constant; a null out-pointer is allowed.
        unsafe {
            editor
                .layout_manager()
                .temporaryAttribute_atCharacterIndex_effectiveRange(
                    NSForegroundColorAttributeName,
                    index,
                    std::ptr::null_mut(),
                )
        }
    }

    /// The text view stores its default text colour itself; only the highlight colour must
    /// stay out of the text storage.
    fn stored_color(editor: &EditorController, index: usize) -> Option<Retained<AnyObject>> {
        // SAFETY: plain getter; an immutable AppKit constant; a null out-pointer is allowed.
        unsafe {
            let storage = editor.text_view().textStorage().expect("a text storage");
            storage.attribute_atIndex_effectiveRange(
                NSForegroundColorAttributeName,
                index,
                std::ptr::null_mut(),
            )
        }
    }

    fn is_tag_color(color: Option<Retained<AnyObject>>) -> bool {
        let blue = NSColor::systemBlueColor();
        let blue: &AnyObject = &blue;
        color
            .and_then(|c| c.downcast::<NSColor>().ok())
            .is_some_and(|c| c.isEqual(Some(blue)))
    }

    /// TextKit 1, no substitutions, highlighting only as temporary attributes, and undo that
    /// restores the text and its colours.
    pub fn editor(ctx: &Ctx) {
        let project = ctx.open();
        let editor = project.editor();
        let text_view = editor.text_view();
        // SAFETY: plain getters.
        assert!(
            unsafe { text_view.layoutManager() }.is_some(),
            "TextKit 1 layout manager"
        );
        assert!(
            text_view.textLayoutManager().is_none(),
            "no TextKit 2 layout manager"
        );
        assert!(
            !text_view.isAutomaticQuoteSubstitutionEnabled(),
            "smart quotes off"
        );
        assert!(
            !text_view.isAutomaticDashSubstitutionEnabled(),
            "smart dashes off"
        );
        assert!(
            !text_view.isAutomaticTextReplacementEnabled(),
            "text replacement off"
        );
        assert!(
            !text_view.isGrammarCheckingEnabled(),
            "grammar checking off"
        );
        assert_eq!(
            text_view.writingToolsBehavior(),
            NSWritingToolsBehavior::None,
            "no Writing Tools"
        );
        // The context menu AppKit builds for a right click keeps editing commands and drops
        // the prose submenus.
        let window = project.project_window();
        let click = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::RightMouseDown,
            text_view.convertPoint_toView(NSPoint::new(5.0, 5.0), None),
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("a right click");
        let menu = text_view.menuForEvent(&click).expect("a context menu");
        let titles: Vec<String> = menu
            .itemArray()
            .iter()
            .map(|i| i.title().to_string())
            .collect();
        assert!(titles.iter().any(|t| t == "Paste"), "{titles:?}");
        for prose in [
            "Spelling and Grammar",
            "Substitutions",
            "Transformations",
            "Font",
        ] {
            assert!(!titles.iter().any(|t| t == prose), "{prose} in {titles:?}");
        }
        print!("(context menu {titles:?}) ");

        // The editor shows the selected request as saved.
        wait_loaded(&project);
        if project.selected_request().is_none() {
            // SAFETY: `newRequest:` takes the sender.
            let _: () = unsafe { msg_send![&*project, newRequest: None::<&AnyObject>] };
        }
        let window = project.project_window();
        window.makeFirstResponder(Some(text_view));
        let file = selected_file(ctx, &project);
        let saved = std::fs::read_to_string(&file).expect("request file");
        assert_eq!(text_view.string().to_string(), saved);
        assert!(text_view.isEditable());
        let utf16 = |byte: usize| saved[..byte].encode_utf16().count();
        let envelope = utf16(saved.find("Envelope").expect("an envelope"));
        assert!(
            is_tag_color(temporary_color(editor, envelope)),
            "tag name coloured"
        );
        assert!(
            !is_tag_color(stored_color(editor, envelope)),
            "highlight colour not in the text storage"
        );
        assert!(
            editor.ruler().visible_lines().contains(&1),
            "ruler sees line 1"
        );
        assert!(!window.isDocumentEdited());

        // Type a new element at the start of the Body line, as the keyboard would.
        let body = saved.find("Body").expect("a Body element");
        let at = utf16(saved[..body].rfind('\n').map_or(0, |i| i + 1));
        // SAFETY: inserting a string at an empty range inside the text.
        unsafe {
            text_view
                .insertText_replacementRange(&NSString::from_str("<new/>"), NSRange::new(at, 0))
        };
        let typed = text_view.string().to_string();
        assert!(typed.contains("<new/>"), "{typed}");
        assert!(
            is_tag_color(temporary_color(editor, at + 1)),
            "typed tag name coloured"
        );
        assert!(
            !is_tag_color(stored_color(editor, at + 1)),
            "typing stores no highlight colour"
        );
        assert!(window.isDocumentEdited(), "edited dot");
        assert!(selected_markers(&project).0, "unsaved marker");

        // Autosave writes what the text view shows.
        wait_until("autosave", || {
            std::fs::read_to_string(&file).is_ok_and(|t| t == typed)
        });
        assert!(!window.isDocumentEdited(), "saved");
        assert!(!selected_markers(&project).0, "marker cleared");

        let undo = text_view
            .undoManager()
            .expect("the text view has an undo manager");
        // Without a run loop, the group AppKit opened for the keystroke is still open.
        while undo.groupingLevel() > 0 {
            undo.endUndoGrouping();
        }
        undo.undo();
        assert_eq!(
            text_view.string().to_string(),
            saved,
            "undo restores the text"
        );
        assert!(
            is_tag_color(temporary_color(editor, envelope)),
            "colours after undo"
        );
        assert!(window.isDocumentEdited(), "undo is an edit");
        wait_until("autosave after undo", || {
            std::fs::read_to_string(&file).is_ok_and(|t| t == saved)
        });

        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// Project menu items that act on the selected request are disabled without one, and
    /// Delete steps aside while a text view has the focus, so ⌘⌫ deletes to the start of the
    /// line there instead of asking to delete the request.
    pub fn menu_validation(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        if project.selected_request().is_none() {
            // SAFETY: `newRequest:` takes the sender.
            let _: () = unsafe { msg_send![&*project, newRequest: None::<&AnyObject>] };
        }
        let request = project.selected_request().expect("a request is selected");
        let menu = ctx
            .app
            .mainMenu()
            .and_then(|m| m.itemWithTitle(&NSString::from_str("Project")))
            .and_then(|i| i.submenu())
            .expect("Project menu");
        let enabled = |title: &str| -> bool {
            let item = menu
                .itemWithTitle(&NSString::from_str(title))
                .expect("a Project menu item");
            // SAFETY: `validateMenuItem:` takes a menu item and returns `BOOL`.
            unsafe { msg_send![&*project, validateMenuItem: &*item] }
        };
        let window = project.project_window();
        assert!(enabled("New Request"), "the WSDL is loaded");

        window.makeFirstResponder(Some(project.editor().text_view()));
        assert!(!enabled("Delete"), "⌘⌫ goes to the editor");
        assert!(enabled("Duplicate") && enabled("Send"), "the rest stay");
        let outline = project.sidebar().outline().expect("the sidebar is built");
        window.makeFirstResponder(Some(outline));
        assert!(
            enabled("Delete"),
            "⌘⌫ deletes the selected request from the sidebar"
        );

        let key = project.key();
        ctx.delegate
            .command("deselect", |app| app.select_request(key, None))
            .expect("deselected");
        for title in ["Duplicate", "Rename", "Delete", "Validate", "Send"] {
            assert!(!enabled(title), "{title} without a selected request");
        }
        ctx.delegate
            .command("reselect", |app| app.select_request(key, Some(request)))
            .expect("reselected");
        autoreleasepool(|_| window.performClose(None));
    }

    /// Format XML (⌃I) is one undo step that keeps the selection on the same text; the settings
    /// window writes the user defaults and the model follows; Save All formats with format on
    /// save, autosave never does.
    pub fn format_xml(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        if project.selected_request().is_none() {
            // SAFETY: `newRequest:` takes the sender.
            let _: () = unsafe { msg_send![&*project, newRequest: None::<&AnyObject>] };
        }
        let editor = project.editor();
        let text_view = editor.text_view();
        let window = project.project_window();
        window.makeFirstResponder(Some(text_view));
        let file = selected_file(ctx, &project);
        // Put back at the end: later checks expect the generated request.
        let original = text_view.string().to_string();
        let undo = text_view
            .undoManager()
            .expect("the text view has an undo manager");
        // Without a run loop, AppKit's per-event undo groups never close, and closing them by
        // hand leaves the undo manager refusing the next registration with an exception. So
        // each step gets a group of its own, as one event would.
        undo.setGroupsByEvent(false);
        let grouped = |step: &dyn Fn()| {
            undo.beginUndoGrouping();
            step();
            undo.endUndoGrouping();
        };
        let set_text = |text: &str| {
            grouped(&|| {
                let len = text_view.string().length();
                // SAFETY: replacing the whole text, a valid range.
                unsafe {
                    text_view.insertText_replacementRange(
                        &NSString::from_str(text),
                        NSRange::new(0, len),
                    )
                };
            })
        };
        let send =
            |action: &std::ffi::CStr, target: Option<&AnyObject>, sender: Option<&AnyObject>| {
                undo.beginUndoGrouping();
                // SAFETY: every action used here takes the sender.
                let sent = unsafe {
                    ctx.app
                        .sendAction_to_from(Sel::register(action), target, sender)
                };
                undo.endUndoGrouping();
                sent
            };
        // Sent to the project's controller, as from its key window; headless runs have none.
        let format = |sender: &AnyObject| send(c"formatXML:", Some(&*project), Some(sender));

        let ugly = "<a><b>xy</b></a>";
        set_text(ugly);
        // Select the `y`.
        text_view.setSelectedRange(NSRange::new(7, 1));
        let edit_menu = ctx
            .app
            .mainMenu()
            .and_then(|m| m.itemWithTitle(&NSString::from_str("Edit")))
            .and_then(|i| i.submenu())
            .expect("Edit menu");
        let item = edit_menu
            .itemWithTitle(&NSString::from_str("Format XML"))
            .expect("Format XML item");
        // SAFETY: `validateMenuItem:` takes a menu item and returns `BOOL`.
        let enabled: bool = unsafe { msg_send![&*project, validateMenuItem: &*item] };
        assert!(enabled, "Format XML with a request open");
        assert!(format(&item), "the project window's controller answers ⌃I");
        let formatted = "<a>\n  <b>xy</b>\n</a>\n";
        assert_eq!(text_view.string().to_string(), formatted);
        let selected = text_view.selectedRange();
        assert_eq!(
            (selected.location, selected.length),
            (formatted.find('y').expect("y"), 1),
            "the selection stays on the same text"
        );
        assert!(window.isDocumentEdited(), "formatting is an edit");
        assert_eq!(undo.undoMenuItemTitle().to_string(), "Undo Format XML");
        undo.undo();
        assert_eq!(text_view.string().to_string(), ugly, "one undo step");

        // A request that is not well-formed is left alone.
        set_text("<a><b></a>");
        format(&item);
        assert_eq!(text_view.string().to_string(), "<a><b></a>");

        // The settings window round trip: controls → defaults → model.
        let settings = ctx.delegate.settings_window();
        send(c"showSettings:", None, None);
        assert!(settings.window().isVisible(), "Settings… opens the window");
        assert_eq!(
            settings.selected(),
            Some(Pane::App),
            "on the app's settings"
        );
        assert_eq!(settings.shown(), FormatSettings::default());
        settings.indent_popup().selectItemAtIndex(3);
        settings.on_save_switch().setState(NSControlStateValueOn);
        let target: &AnyObject = settings;
        send(c"settingChanged:", Some(target), None);
        let reread = NSUserDefaults::initWithSuiteName(
            NSUserDefaults::alloc(),
            Some(&NSString::from_str(DEFAULTS_SUITE)),
        )
        .expect("the scratch suite");
        assert_eq!(
            reread.integerForKey(&NSString::from_str(washboard_app::INDENT_KEY)),
            4
        );
        assert!(reread.boolForKey(&NSString::from_str(washboard_app::ON_SAVE_KEY)));
        let model = ctx.delegate.read(|app| app.format_settings());
        assert_eq!(
            model,
            Some(FormatSettings {
                indent: 4,
                on_save: true
            })
        );
        settings.window().orderOut(None);

        // Autosave writes the text as typed, also with format on save.
        window.makeFirstResponder(Some(text_view));
        set_text(ugly);
        wait_until("autosave", || {
            std::fs::read_to_string(&file).is_ok_and(|t| t == ugly)
        });
        // Save All formats first, at the new width.
        set_text(ugly);
        send(c"saveAll:", None, None);
        let wide = "<a>\n    <b>xy</b>\n</a>\n";
        assert_eq!(text_view.string().to_string(), wide);
        assert_eq!(std::fs::read_to_string(&file).ok().as_deref(), Some(wide));
        assert!(!window.isDocumentEdited(), "saved");

        ctx.delegate.set_format_settings(FormatSettings::default());
        set_text(&original);
        send(c"saveAll:", None, None);
        assert_eq!(std::fs::read_to_string(&file).ok(), Some(original));
        autoreleasepool(|_| window.performClose(None));
    }

    /// The file of the request selected in `project`.
    fn selected_file(ctx: &Ctx, project: &ProjectWindowController) -> PathBuf {
        let name = selected_row(project).title();
        ctx.project.join("requests").join(format!("{name}.xml"))
    }

    /// The sidebar row of the request selected in `project`.
    fn selected_row(project: &ProjectWindowController) -> Retained<SidebarNode> {
        let id = project.selected_request().expect("a selected request");
        all_nodes(&project.sidebar().roots())
            .into_iter()
            .find(|n| n.request() == Some(id))
            .expect("the selected request's row")
    }

    /// The (unsaved, invalid) markers of the selected request's row.
    fn selected_markers(project: &ProjectWindowController) -> (bool, bool) {
        selected_row(project).markers()
    }

    /// Lays out a 1 MB request and prints how long it took (PLAN §4 "Editor": typing must stay
    /// responsive in a 1 MB file). Printed, not asserted: runner speed varies.
    pub fn editor_1mb_layout(_ctx: &Ctx) {
        let mut text = String::from("<cus:list xmlns:cus=\"urn:example:customer\">\n");
        let mut n = 0;
        while text.len() < 1 << 20 {
            text.push_str(&format!(
                "  <cus:item id=\"{n}\"><cus:name>Customer {n}</cus:name></cus:item>\n"
            ));
            n += 1;
        }
        text.push_str("</cus:list>\n");

        let mtm = MainThreadMarker::new().expect("main thread");
        let editor = EditorController::new(mtm);
        let started = Instant::now();
        editor.set_text(&text);
        let highlighted = started.elapsed();
        let text_view = editor.text_view();
        // SAFETY: plain getter.
        let container = unsafe { text_view.textContainer() }.expect("a text container");
        editor
            .layout_manager()
            .ensureLayoutForTextContainer(&container);
        let laid_out = started.elapsed();
        print!(
            "({} KB, {n} lines: set and highlight {} ms, full layout {} ms) ",
            text.len() / 1024,
            highlighted.as_millis(),
            (laid_out - highlighted).as_millis()
        );

        // One keystroke in the middle: the incremental path.
        let middle = text.len() / 2;
        let at = text[middle..].find('<').map_or(middle, |i| middle + i);
        let started = Instant::now();
        // SAFETY: inserting a string at an empty range inside the text.
        unsafe {
            text_view.insertText_replacementRange(&NSString::from_str("x"), NSRange::new(at, 0))
        };
        print!("(keystroke {} ms) ", started.elapsed().as_millis());
    }

    /// The editor's colours against its background, in both appearances, and the project
    /// window under Dark Mode. Contrast is the WCAG ratio of the colours as AppKit resolves
    /// them for each appearance; secondary label colours are blended over the background.
    pub fn dark_mode(ctx: &Ctx) {
        // SAFETY: immutable AppKit constants.
        let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
        let mut colours: Vec<(String, Retained<NSColor>)> = washboard_app::highlight_palette()
            .into_iter()
            .map(|(kind, colour)| (format!("{kind:?}"), colour))
            .collect();
        colours.push(("text".into(), NSColor::textColor()));
        colours.push(("ruler line number".into(), NSColor::secondaryLabelColor()));
        colours.push(("error marker".into(), NSColor::systemOrangeColor()));
        for (name, minimum) in [(aqua, None), (dark, Some(3.0))] {
            let appearance = NSAppearance::appearanceNamed(name).expect("a system appearance");
            let ratios = RefCell::new(Vec::new());
            let block = RcBlock::new(|| {
                let background = srgb(&NSColor::textBackgroundColor());
                for (what, colour) in &colours {
                    ratios
                        .borrow_mut()
                        .push((what.clone(), contrast(srgb(colour), background)));
                }
            });
            appearance.performAsCurrentDrawingAppearance(&block);
            drop(block);
            let ratios = ratios.into_inner();
            print!("({name}: ");
            for (what, ratio) in &ratios {
                print!("{what} {ratio:.1} ");
            }
            print!(") ");
            if let Some(minimum) = minimum {
                for (what, ratio) in &ratios {
                    assert!(*ratio >= minimum, "{what} in {name}: contrast {ratio:.2}");
                }
            }
        }

        // The project window follows the app into Dark Mode while open.
        let project = ctx.open();
        wait_loaded(&project);
        let text_view = project.editor().text_view();
        ctx.app
            .setAppearance(NSAppearance::appearanceNamed(dark).as_deref());
        let window = project.project_window();
        window.displayIfNeeded();
        let shown = text_view.effectiveAppearance().name().to_string();
        ctx.app.setAppearance(None);
        assert_eq!(
            shown,
            dark.to_string(),
            "the editor follows the app's appearance"
        );
        autoreleasepool(|_| window.performClose(None));
    }

    /// Red, green, blue and alpha of `colour` in sRGB, as resolved for the current drawing
    /// appearance.
    fn srgb(colour: &NSColor) -> [f64; 4] {
        let c = colour
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .expect("an sRGB colour");
        [
            c.redComponent(),
            c.greenComponent(),
            c.blueComponent(),
            c.alphaComponent(),
        ]
    }

    /// WCAG 2 contrast ratio of `fg` drawn over the opaque `bg`.
    fn contrast(fg: [f64; 4], bg: [f64; 4]) -> f64 {
        let blend = |i: usize| fg[i] * fg[3] + bg[i] * (1.0 - fg[3]);
        let linear = |c: f64| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let luminance = |rgb: [f64; 3]| {
            0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
        };
        let a = luminance([blend(0), blend(1), blend(2)]);
        let b = luminance([bg[0], bg[1], bg[2]]);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// An invalid value in a fixture request is listed, marked in the gutter and on the
    /// sidebar row, and a click selects it; fixing it and validating clears all three.
    pub fn diagnostics(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        let editor = project.editor();
        let text_view = editor.text_view();
        let issues = project.issues().table();
        let text = text_view.string().to_string();
        let start = text.find("<asOf>").expect("the template has asOf") + "<asOf>".len();
        let end = start + text[start..].find("</asOf>").expect("asOf is closed");
        let date = text[start..end].to_owned();
        // The template is ASCII up to here, so byte offsets are UTF-16 offsets.
        assert!(text[..end].is_ascii());
        let line = text[..start].matches('\n').count() + 1;
        wait_until("the first check", || issues.rows().is_empty());
        wait_until("the text to be validated", || {
            project.issues().summary() == "✓ Valid"
        });

        // The request bar names the open request and its operation, and says the text is
        // well-formed.
        let bar = project.request_bar();
        assert_eq!(bar.name(), selected_row(&project).title());
        assert_eq!(bar.operation().as_deref(), Some("SOAP 1.1 · Lookup"));
        assert_eq!(bar.state(), "Well-formed");
        project.project_window().layoutIfNeeded();
        let chip = bar.operation_chip();
        assert!(
            chip.frame().size.width > 60.0,
            "the chip shows its text: {:?}",
            chip.frame()
        );

        let replace = |at: usize, len: usize, with: &str| {
            // SAFETY: replacing a range inside the text, as typing over a selection does.
            unsafe {
                text_view
                    .insertText_replacementRange(&NSString::from_str(with), NSRange::new(at, len))
            };
        };
        replace(start, date.len(), "someday");
        // Still well-formed: until the schema check runs, the text is neither valid nor
        // invalid.
        wait_until("the edit to read as unchecked", || {
            project.issues().summary() == "Checking…"
        });
        wait_until("the invalid date to be listed", || {
            !issues.rows().is_empty()
        });
        assert_eq!(issues.rows()[0][0], line.to_string(), "{:?}", issues.rows());
        let summary = project.issues().summary();
        assert!(summary.starts_with("⚠ "), "{summary}");
        assert!(
            editor.ruler().error_lines().contains(&line),
            "gutter marker"
        );
        wait_until("the invalid marker", || selected_markers(&project).1);

        issues.click(0);
        let selected = text_view.selectedRange();
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        assert!(
            (line_start..start + "someday".len() + "</asOf>".len()).contains(&selected.location),
            "click selects the issue: {selected:?}"
        );

        // Broken XML turns the request bar's state into the error's line; the schema error
        // before it did not.
        assert_eq!(bar.state(), "Well-formed");
        replace(start, "someday".len(), "<");
        wait_until("the XML error in the request bar", || {
            bar.state() == format!("XML error, line {line}")
        });

        replace(start, 1, &date);
        wait_until("the request bar to read well-formed again", || {
            bar.state() == "Well-formed"
        });
        let validate = std::ffi::CString::new("validateRequest:").expect("a selector name");
        // SAFETY: the window controller's actions take the sender.
        let sent = unsafe {
            ctx.app
                .sendAction_to_from(Sel::register(&validate), Some(&project), None)
        };
        assert!(sent);
        wait_until("the issues to clear", || issues.rows().is_empty());
        wait_until("the text to read as valid again", || {
            project.issues().summary() == "✓ Valid"
        });
        assert!(editor.ruler().error_lines().is_empty(), "gutter cleared");
        wait_until("the marker to clear", || !selected_markers(&project).1);
        autoreleasepool(|_| project.project_window().performClose(None));
    }

    const LOOKUP_RESPONSE: &str = "<soapenv:Envelope \
        xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"><soapenv:Body>\
        <tns:LookupResponse xmlns:tns=\"urn:example:legacy\"><result><street>Main St 1</street>\
        <city>Vienna</city></result></tns:LookupResponse></soapenv:Body></soapenv:Envelope>";

    /// Reads one HTTP request: its head and, with a `Content-Length`, its body.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = stream.read(&mut buf).expect("read the request");
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&data[..end]).into_owned();
            let length = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                break;
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    /// A SOAP server on this machine that answers one request with `body` and returns what
    /// it received.
    fn serve_once(body: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let port = listener.local_addr().expect("local address").port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("read timeout");
            let request = read_request(&mut stream);
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).expect("reply");
            request
        });
        (port, handle)
    }

    /// A server that accepts a connection and never answers.
    fn serve_nothing() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let port = listener.local_addr().expect("local address").port();
        std::thread::spawn(move || {
            let connection = listener.accept();
            std::thread::sleep(Duration::from_secs(20));
            drop(connection);
        });
        port
    }

    fn local_server(id: ServerId, port: u16) -> Server {
        Server {
            id,
            name: "Local".into(),
            url: format!("http://127.0.0.1:{port}/legacy"),
            ignore_tls_errors: false,
            auth: Auth::Basic {
                username: "alice".into(),
            },
            timeout: Duration::from_secs(5),
        }
    }

    /// A refused send lists the issues; a send fills the response pane, the history and the
    /// log; a history entry restores the request; Cancel returns to Send.
    pub fn send(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        let response = project.response();
        let tabs: Vec<String> = response
            .tabs()
            .tabViewItems()
            .iter()
            .map(|t| t.label().to_string())
            .collect();
        assert_eq!(tabs, washboard_app::RESPONSE_TABS);
        let action = |name: &str| {
            let name = std::ffi::CString::new(name).expect("a selector name");
            // SAFETY: the window controller's actions take the sender.
            let sent = unsafe {
                ctx.app
                    .sendAction_to_from(Sel::register(&name), Some(&project), None)
            };
            assert!(sent, "{name:?} reached the window controller");
        };
        let send_label = || project.send_item().map(|i| i.label().to_string());

        // A server on this machine, with Basic auth so the log has an `Authorization` header.
        let key = project.key();
        let (port, server) = serve_once(LOOKUP_RESPONSE);
        let id = ctx
            .delegate
            .command("add a server", |app| {
                let id = app.add_server(key)?;
                app.update_server(key, &local_server(id, port), Some("secret"))?;
                Ok(id)
            })
            .expect("server added");
        let popup = project.server_popup();
        let local = popup
            .itemTitles()
            .iter()
            .position(|t| t.to_string() == "Local")
            .expect("the popup lists the new server");
        popup.selectItemAtIndex(local as isize);
        action("chooseServer:");

        // An invalid request is not sent.
        let editor = project.editor();
        let text_view = editor.text_view();
        let text = text_view.string().to_string();
        let start = text.find("<asOf>").expect("the template has asOf") + "<asOf>".len();
        let end = start + text[start..].find("</asOf>").expect("asOf is closed");
        let date = text[start..end].to_owned();
        assert!(text[..end].is_ascii());
        let replace = |at: usize, len: usize, with: &str| {
            // SAFETY: replacing a range inside the text, as typing over a selection does.
            unsafe {
                text_view
                    .insertText_replacementRange(&NSString::from_str(with), NSRange::new(at, len))
            };
        };
        let history = response.history().rows().len();
        project.issues().table().view().setHidden(true);
        replace(start, date.len(), "someday");
        action("sendRequest:");
        wait_until("the refusal", || {
            send_label().as_deref() == Some("Send") && !project.issues().table().rows().is_empty()
        });
        assert!(!project.issues().table().view().isHidden(), "issues shown");
        assert_eq!(response.history().rows().len(), history, "nothing sent");

        replace(start, "someday".len(), &date);
        let project_menu = ctx
            .app
            .mainMenu()
            .and_then(|m| m.itemWithTitle(&NSString::from_str("Project")))
            .and_then(|i| i.submenu())
            .expect("Project menu");
        let enabled = |title: &str| -> bool {
            let item = project_menu
                .itemWithTitle(&NSString::from_str(title))
                .expect("a Project menu item");
            // SAFETY: `validateMenuItem:` takes a menu item and returns `BOOL`.
            unsafe { msg_send![&*project, validateMenuItem: &*item] }
        };
        assert!(enabled("Send") && !enabled("Cancel Send"), "idle");
        assert!(!response.is_spinning());
        action("sendRequest:");
        assert_eq!(send_label().as_deref(), Some("Cancel"));
        assert_eq!(response.status(), "Sending…");
        assert!(response.is_spinning(), "the spinner shows while sending");
        // The menu's Send doesn't cancel: ⌘↩ twice must not stop the first send.
        assert!(!enabled("Send") && enabled("Cancel Send"), "sending");
        wait_until("the response", || response.status().starts_with("200"));
        assert_eq!(send_label().as_deref(), Some("Send"));
        assert!(!response.is_spinning());
        let body = response.body().text_view().string().to_string();
        assert!(body.contains("LookupResponse"), "{body}");
        // The pane shows the model's headers; what they are is the HTTP client's business,
        // tested in washboard-core.
        let headers = ctx
            .delegate
            .read(|app| Some(app.project(key)?.response()?.headers.clone()))
            .flatten()
            .expect("a response");
        assert!(!headers.is_empty());
        let headers_font = response
            .headers_view()
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
            .and_then(|v| v.font())
            .expect("the Headers tab has a font");
        assert!(headers_font.isFixedPitch(), "headers in the code font");
        for (name, value) in &headers {
            let line = format!("{name}: {value}");
            assert!(
                response.headers().contains(&line),
                "{line} in {}",
                response.headers()
            );
        }
        assert_eq!(
            response.history().rows().len(),
            history + 1,
            "history row added"
        );
        // Which server and when, in the status line and the history row.
        let status = response.status();
        assert!(status.contains(" · Local · "), "{status}");
        assert!(
            status
                .split(" · ")
                .last()
                .is_some_and(|sent| !sent.is_empty()),
            "{status}"
        );
        let newest = &response.history().rows()[0];
        assert_eq!(newest[1], "Local", "{newest:?}");
        assert!(newest[3].ends_with(" ms"), "{newest:?}");
        server.join().expect("the server thread");
        assert!(!ctx.delegate.http_log().table().rows().is_empty(), "logged");

        // Restore Request puts the sent request back.
        replace(start, date.len(), "2031-12-31");
        let history_table = response.history();
        history_table
            .table()
            .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        history_table.click(0);
        // SAFETY: `restoreRequest:` takes the sender.
        let _: () = unsafe { msg_send![response, restoreRequest: None::<&AnyObject>] };
        assert_eq!(text_view.string().to_string(), text, "restored");

        // Cancel stops waiting for a server that never answers.
        let port = serve_nothing();
        ctx.delegate
            .command("point the server elsewhere", |app| {
                app.update_server(key, &local_server(id, port), None)
            })
            .expect("server updated");
        action("sendRequest:");
        assert_eq!(send_label().as_deref(), Some("Cancel"));
        action("sendRequest:");
        assert_eq!(send_label().as_deref(), Some("Send"), "cancelled");
        // Project ▸ Cancel Send (⌘.) does the same.
        action("sendRequest:");
        assert_eq!(send_label().as_deref(), Some("Cancel"));
        action("cancelSend:");
        assert_eq!(
            send_label().as_deref(),
            Some("Send"),
            "cancelled from the menu"
        );
        assert!(
            response.status().starts_with("200"),
            "the last response stays"
        );
        assert_eq!(response.history().rows().len(), history + 1);
        assert!(ctx.alerts().is_empty(), "{:?}", ctx.alerts());

        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// One log panel for the app, opened from the menu action, listing the model's exchanges
    /// with `Authorization` masked until revealed.
    pub fn http_log(ctx: &Ctx) {
        // SAFETY: `showHttpLog:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, showHttpLog: None::<&AnyObject>] };
        let log = ctx.delegate.http_log();
        assert!(log.panel().isVisible(), "log panel shown");
        assert_eq!(log.table().rows().len(), 1, "the send check's exchange");

        // The panel shows the model's request headers, masked or revealed; which header is
        // masked and how is the model's business, tested in washboard-ui-model.
        let headers = |revealed: bool| {
            ctx.delegate
                .read(|app| app.http_log().front().map(|e| e.request_headers(revealed)))
                .flatten()
                .expect("a log entry")
        };
        let shows = |revealed: bool| {
            let text = log.request_text();
            headers(revealed)
                .iter()
                .all(|(name, value)| text.contains(&format!("{name}: {value}")))
        };
        assert_ne!(
            headers(false),
            headers(true),
            "the send had something to mask"
        );
        assert!(shows(false), "masked: {}", log.request_text());
        log.set_revealed(true);
        assert!(shows(true), "revealed: {}", log.request_text());
        log.set_revealed(false);
        assert!(shows(false), "masked again: {}", log.request_text());
        // The send went to a plain-HTTP local server; the TLS wording is tested on Linux.
        assert_eq!(log.tls_text(), "No TLS (plain HTTP)");
        log.panel().close();
    }

    /// A table reports the row the user picks with ↑/↓ as it does a click, so a detail view
    /// (the Servers form, the HTTP log, history) follows the highlight. Selection made by
    /// code is not reported.
    pub fn table_keys(_ctx: &Ctx) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let table = TextTable::new(&["Name"], mtm);
        let picked = Rc::new(RefCell::new(Vec::new()));
        let log = picked.clone();
        table.on_click(move |row| log.borrow_mut().push(row));
        table.set_rows(vec![vec!["a".into()], vec!["b".into()], vec!["c".into()]]);
        let view = table.table();
        view.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        assert!(
            picked.borrow().is_empty(),
            "selection by code is not reported"
        );
        let down = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::NumericPad | NSEventModifierFlags::Function,
            0.0,
            0,
            None,
            &NSString::from_str("\u{F701}"),
            &NSString::from_str("\u{F701}"),
            false,
            125,
        )
        .expect("a ↓ key event");
        view.keyDown(&down);
        assert_eq!(view.selectedRow(), 1, "↓ moved the selection");
        assert_eq!(*picked.borrow(), [1], "and reported it");
        table.click(1);
        assert_eq!(
            *picked.borrow(),
            [1, 1],
            "a click on the selected row still counts"
        );
    }

    /// Pumps the main run loop until `done` holds; sheets attach and detach asynchronously.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
    }

    /// Completion inside the Lookup body element asks the model for its range and items, and
    /// the tool tip on an element name is the model's hover text.
    pub fn completion_and_hover(ctx: &Ctx) {
        let project = ctx.open();
        wait_loaded(&project);
        let key = project.key();
        ctx.delegate
            .command("new request", |app| {
                let op = app.default_operation(key)?;
                app.new_request(key, &op)
            })
            .expect("a Lookup request");
        let editor = project.editor();
        let text_view = editor.text_view();
        project
            .project_window()
            .makeFirstResponder(Some(text_view.as_ref()));
        let text = text_view.string().to_string();
        assert!(text.is_ascii(), "offsets below count bytes as UTF-16 units");
        let as_of = text.find("<asOf>").expect("the template has asOf");

        // Typing `<` before `<asOf>` goes through the model; typing it in the view would
        // open the completion window, which a headless run cannot dismiss.
        let edit = |range: std::ops::Range<usize>, with: &'static str| {
            ctx.delegate
                .command("edit", |app| app.edit(key, range, with))
                .expect("edited");
            editor.show_model_text();
        };
        edit(as_of..as_of, "<");
        text_view.setSelectedRange(NSRange::new(as_of + 1, 0));
        // SAFETY: `rangeForUserCompletion` takes nothing and returns a range.
        let range: NSRange = unsafe { msg_send![text_view, rangeForUserCompletion] };
        assert_eq!((range.location, range.length), (as_of + 1, 0));
        let mut index: NSInteger = 0;
        let empty = NSArray::<NSString>::new();
        // SAFETY: the delegate method's signature; `index` outlives the call.
        let words: Retained<NSArray<NSString>> = unsafe {
            msg_send![
                editor,
                textView: text_view,
                completions: &*empty,
                forPartialWordRange: range,
                indexOfSelectedItem: &mut index
            ]
        };
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        assert_eq!(words, ["customerNo", "asOf"]);
        text_view.setSelectedRange(NSRange::new(0, 4));
        assert!(
            editor.completions_at_cursor().is_none(),
            "not with a selection"
        );
        edit(as_of..as_of + 1, "");

        let hover = editor
            .show_tool_tip_at(as_of + 2)
            .expect("asOf has a hover");
        assert!(hover.starts_with("asOf\n"), "{hover}");
        assert!(hover.contains("exactly 1"), "{hover}");
        // SAFETY: the tool tip owner's method; the user data is unused.
        let shown: Retained<NSString> = unsafe {
            msg_send![
                editor,
                view: AsRef::<NSView>::as_ref(text_view),
                stringForToolTip: 0 as NSInteger,
                point: NSPoint::new(0.0, 0.0),
                userData: std::ptr::null_mut::<std::ffi::c_void>()
            ]
        };
        assert_eq!(shown.to_string(), hover);
        assert_eq!(editor.show_tool_tip_at(1), None, "no hover on the envelope");
        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// The model's server names for `key`, in order.
    fn server_names(ctx: &Ctx, key: ProjectKey) -> Vec<String> {
        ctx.delegate
            .read(|app| {
                app.project(key)
                    .map(|w| w.servers().iter().map(|s| s.name.clone()).collect())
            })
            .flatten()
            .unwrap_or_default()
    }

    fn table_column(table: &washboard_app::TextTable, column: usize) -> Vec<String> {
        table
            .rows()
            .into_iter()
            .map(|r| r[column].clone())
            .collect()
    }

    /// A table cell's text sits inside the cell, and its content (the text, or the stack holding
    /// it) is centred vertically in the cell.
    fn assert_fits(cell: &NSView, what: &str) {
        cell.layoutSubtreeIfNeeded();
        let label = cell
            .downcast_ref::<NSTableCellView>()
            // SAFETY: a cell's text field is one of its subviews, so the cell keeps it alive.
            .and_then(|c| unsafe { c.textField() })
            .expect("cells are NSTableCellViews with a text field");
        let outer = cell.bounds();
        let inner = label.convertRect_toView(label.bounds(), Some(cell));
        assert!(
            inner.origin.x >= -0.5 && inner.origin.x + inner.size.width <= outer.size.width + 0.5,
            "{what} within its cell: {inner:?} in {outer:?}"
        );
        let content = cell.subviews().firstObject().expect("the cell has content");
        let content = content.frame();
        let mid = |r: NSRect| r.origin.y + r.size.height / 2.0;
        assert!(
            (mid(content) - mid(outer)).abs() < 2.0,
            "{what} centred vertically: {content:?} in {outer:?}"
        );
    }

    /// The Servers pane's form rows sit together, and its sections fit the window.
    fn assert_settings_layout(settings: &ServersPane) {
        let window = settings.view().window().expect("the pane is shown");
        window
            .contentView()
            .expect("content")
            .layoutSubtreeIfNeeded();
        let frame = |v: &NSView| v.convertRect_toView(v.bounds(), None);
        let fields: [&NSView; 4] = [
            settings.name_field(),
            settings.url_field(),
            settings.user_field(),
            settings.password_field(),
        ];
        // Name, URL, then TLS and Auth, then User, Password: never more than three rows apart.
        for pair in fields.windows(2) {
            let gap = frame(pair[0]).origin.y - frame(pair[1]).origin.y;
            assert!(
                gap > 0.0 && gap < 3.0 * 50.0,
                "form rows stay together: {gap} between {:?} and {:?}",
                pair[0].frame(),
                pair[1].frame()
            );
        }
        let width = window.contentLayoutRect().size.width;
        for field in fields {
            let f = frame(field);
            assert!(
                f.origin.x > 0.0 && f.origin.x + f.size.width < width,
                "a field inside the window: {f:?} in width {width}"
            );
        }
        let list = settings.table().view().frame();
        assert!(
            list.size.height >= 100.0,
            "the server list has room: {list:?}"
        );
    }

    /// Waits for the import check the last file change started.
    fn wait_checked(sheet: &ImportSheetController) {
        wait_until("the import check", || {
            !sheet.status().starts_with("Checking") && !sheet.references().rows().is_empty()
        });
    }

    /// `fixtures/customer`'s WSDLs and schemas, entry first.
    const CUSTOMER_FILES: [&str; 6] = [
        "CustomerService.wsdl",
        "CustomerBinding.wsdl",
        "xsd/customer.xsd",
        "xsd/common/party.xsd",
        "xsd/common/party-ids.xsd",
        "xsd/ext/audit.xsd",
    ];

    /// `fixtures/customer`'s WSDLs and schemas without `xsd/common/party-ids.xsd`, which
    /// `party.xsd` includes.
    fn customer_without_include(dir: &Path) -> PathBuf {
        let from = fixtures().join("customer");
        for file in CUSTOMER_FILES
            .into_iter()
            .filter(|f| *f != "xsd/common/party-ids.xsd")
        {
            let to = dir.join(file);
            std::fs::create_dir_all(to.parent().expect("a parent")).expect("mkdir");
            std::fs::copy(from.join(file), &to).expect("copy fixture");
        }
        dir.to_owned()
    }

    /// Create stays disabled while a reference is unresolved and enables once the missing
    /// file is added; Create opens the project with its settings, offering the WSDL's address
    /// as a server.
    pub fn new_project_sheet(ctx: &Ctx) {
        let tmp = TempDir::new().expect("temp dir");
        let files = customer_without_include(&tmp.path().join("incomplete"));
        let projects = tmp.path().join("projects");
        std::fs::create_dir(&projects).expect("mkdir");

        // SAFETY: `newProject:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, newProject: None::<&AnyObject>] };
        let sheet = ctx
            .delegate
            .new_project_sheet()
            .expect("created by newProject:");
        let window = sheet.window().retain();
        wait_until("the window to show", || window.isVisible());
        assert!(
            window.sheetParent().is_none(),
            "its own window, not a sheet"
        );
        assert_eq!(window.tabbingMode(), NSWindowTabbingMode::Disallowed);
        // SAFETY: `newProject:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, newProject: None::<&AnyObject>] };
        assert!(
            ctx.delegate
                .new_project_sheet()
                .is_some_and(|again| std::ptr::eq(&*again, &*sheet)),
            "a second New Project brings the first forward"
        );
        assert!(!sheet.finish_button().isEnabled(), "nothing chosen yet");
        assert_eq!(sheet.status(), "Choose the WSDL.");

        // A long location is shortened in the form instead of widening it past the window.
        let deep = (0..12).fold(tmp.path().to_path_buf(), |p, i| {
            p.join(format!("a-rather-long-folder-name-{i}"))
        });
        std::fs::create_dir_all(&deep).expect("mkdir");
        sheet.choose_location(deep);
        window.layoutIfNeeded();
        let content = window.contentView().expect("content view").bounds();
        let in_window = |v: &NSView| v.convertRect_toView(v.bounds(), None);
        let inside = |what: &str, r: NSRect| {
            assert!(
                r.origin.x >= 19.5
                    && r.origin.x + r.size.width <= content.size.width - 19.5
                    && r.origin.y >= 19.5
                    && r.origin.y + r.size.height <= content.size.height - 19.5
                    && r.size.width > 0.0
                    && r.size.height > 0.0,
                "{what} inside the margins of {content:?}: {r:?}"
            );
        };
        let location = in_window(sheet.location());
        inside("the location", location);
        assert!(
            location.size.width > 200.0,
            "the location takes the row's width: {location:?}"
        );
        inside("Create", in_window(sheet.finish_button()));
        let name = in_window(sheet.name_field());
        assert!(
            name.origin.x < 150.0,
            "the form starts at the leading edge: {name:?}"
        );
        let references = in_window(sheet.references().view());
        inside("the references", references);
        assert!(references.size.height >= 119.5, "{references:?}");
        inside("the findings", in_window(sheet.messages().view()));

        sheet.set_name("Customers");
        sheet.choose_location(projects.clone());
        sheet.choose_wsdl(files.join("CustomerService.wsdl"));
        sheet.add_files(vec![files.clone()]);
        wait_checked(&sheet);
        let rows = sheet.references().rows();
        let table = sheet.references().table();
        for column in 0..table.numberOfColumns() {
            let cell = table
                .viewAtColumn_row_makeIfNecessary(column, 0, true)
                .expect("a cell");
            assert_fits(&cell, "a reference");
        }
        let missing: Vec<&Vec<String>> = rows.iter().filter(|r| r[0] == "✗").collect();
        assert_eq!(missing.len(), 1, "{rows:?}");
        assert_eq!(missing[0][1], "xs:include party-ids.xsd");
        assert_eq!(missing[0][2], "not supplied");
        assert!(!sheet.messages().rows().is_empty(), "the finding is listed");
        assert!(!sheet.finish_button().isEnabled(), "disabled while ✗");
        let open = ctx.delegate.projects().len();
        sheet.finish();
        assert_eq!(
            ctx.delegate.projects().len(),
            open,
            "disabled Create does nothing"
        );

        sheet.add_files(vec![fixtures().join("customer/xsd/common/party-ids.xsd")]);
        wait_checked(&sheet);
        assert!(
            table_column(sheet.references(), 0).iter().all(|m| m == "✓"),
            "{:?}",
            sheet.references().rows()
        );
        assert!(sheet.finish_button().isEnabled(), "enabled once all ✓");

        sheet.finish();
        wait_until("the window to hide", || !window.isVisible());
        let project = ctx
            .delegate
            .projects()
            .into_iter()
            .find(|p| p.name() == "Customers")
            .expect("Create opens the project");
        // The files come from two folder trees, so they keep their full paths under `wsdl/`.
        assert!(projects.join("Customers/wsdl").is_dir());

        let window = project.project_window();
        // Opened on the project's Servers, to confirm the suggested server.
        let settings_window = ctx.delegate.settings_window();
        assert!(settings_window.window().isVisible());
        assert_eq!(
            settings_window.selected(),
            Some(Pane::Servers(project.key()))
        );
        let settings = settings_window.servers(project.key());
        assert!(settings.servers().is_empty(), "nothing before confirming");
        let suggested = settings.suggestion_rows();
        assert_eq!(suggested.len(), 1, "the SOAP 1.1 port: {suggested:?}");
        assert!(settings.shows_suggestions());
        assert_settings_layout(&settings);
        settings.confirm_suggestion(0);
        assert!(settings.suggestion_rows().is_empty());
        assert!(!settings.shows_suggestions(), "nothing left to suggest");
        assert_eq!(server_names(ctx, project.key()), [suggested[0][0].clone()]);
        assert_eq!(settings.selected(), Some(0));
        assert_eq!(
            settings.url_field().stringValue().to_string(),
            suggested[0][1]
        );

        autoreleasepool(|_| window.performClose(None));
        assert!(
            !settings_window
                .sidebar_titles()
                .contains(&"Customers".to_string()),
            "the closed project's section is gone"
        );
        settings_window.window().orderOut(None);

        // The close button cancels, like Cancel.
        let sheet = ctx.delegate.show_new_project_sheet();
        let window = sheet.window().retain();
        wait_until("the window to show", || window.isVisible());
        autoreleasepool(|_| window.performClose(None));
        assert!(!window.isVisible());
        assert_eq!(
            ctx.delegate
                .read(|app| app.import_sheet(ImportTarget::NewProject).is_some()),
            Some(false),
            "closing the window cancels the import"
        );
    }

    /// Replace WSDL, from the project's General settings, checks the new files like New
    /// Project, swaps the WSDL and says what changed; the pane then lists the new files.
    pub fn replace_wsdl(ctx: &Ctx) {
        let key = ctx
            .delegate
            .open_project_at(&ctx.other)
            .expect("the other project opens");
        let project = ctx.delegate.project(key).expect("a window");
        wait_loaded(&project);
        let window = project.project_window();
        let settings = ctx.delegate.show_settings(Some(Pane::ProjectGeneral(key)));
        let settings_window = settings.window().retain();
        let before = settings.wsdl_files_shown().expect("the General pane");
        assert_eq!(before, ["Legacy.wsdl"]);
        // Show in Finder selects the entry WSDL in the project's folder (Finder itself is
        // not opened here).
        let shown = settings.wsdl_to_show(key).expect("the entry WSDL");
        let expected = ctx.other.join("wsdl/Legacy.wsdl");
        assert_eq!(
            shown.canonicalize().expect("exists"),
            expected.canonicalize().expect("exists")
        );
        // SAFETY: `replaceWsdl:` takes the sender.
        let _: () = unsafe { msg_send![settings, replaceWsdl: None::<&AnyObject>] };
        wait_until("the sheet to attach", || {
            settings_window.attachedSheet().is_some()
        });
        assert!(window.attachedSheet().is_none(), "on the Settings window");
        let sheet = project.replace_sheet().expect("created by replaceWsdl:");
        assert_eq!(sheet.finish_button().title().to_string(), "Replace");
        assert!(!sheet.finish_button().isEnabled());

        let customer = fixtures().join("customer");
        sheet.choose_wsdl(customer.join("CustomerService.wsdl"));
        sheet.add_files(vec![customer]);
        wait_checked(&sheet);
        assert!(
            sheet.finish_button().isEnabled(),
            "{:?}",
            sheet.references().rows()
        );
        sheet.finish();
        wait_until("the outcome", || project.replace_summary().is_some());
        let summary = project.replace_summary().expect("shown");
        assert!(summary.contains("removed"), "{summary}");
        wait_until("the outcome alert", || {
            settings_window.attachedSheet().is_some()
        });
        let alert = settings_window.attachedSheet().expect("the outcome alert");
        settings_window.endSheet(&alert);
        wait_until("the new services", || {
            project.sidebar().roots()[1]
                .children()
                .iter()
                .any(|n| n.title() == "CustomerService")
        });
        // The pane lists the new set, the entry WSDL first, and not the previous set kept in
        // `wsdl/.previous`.
        let files = settings.wsdl_files_shown().expect("the General pane");
        assert_eq!(files[0], "CustomerService.wsdl", "{files:?}");
        assert!(files.iter().all(|f| !f.starts_with('.')), "{files:?}");
        autoreleasepool(|_| settings_window.performClose(None));
        autoreleasepool(|_| window.performClose(None));
    }

    /// Settings… opens the window on the app's settings; Project Settings… on the project's
    /// Servers, which edits the model's servers without a Done button: rename, Basic auth with
    /// a password in the secret store, + and −. Closing the project removes its section; the
    /// edits survive reopening it.
    pub fn settings_window(ctx: &Ctx) {
        let project = ctx.open();
        let key = project.key();
        let window = project.project_window();
        // Nothing remembered: Settings… opens on the app's settings.
        ctx.delegate
            .defaults()
            .removeObjectForKey(&NSString::from_str(washboard_app::PANE_KEY));
        // SAFETY: `showSettings:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, showSettings: None::<&AnyObject>] };
        let settings = ctx.delegate.settings_window();
        let settings_window = settings.window().retain();
        assert!(settings_window.isVisible(), "Settings… opens the window");
        assert!(settings_window.attachedSheet().is_none() && window.attachedSheet().is_none());
        assert_eq!(settings.selected(), Some(Pane::App));
        assert_eq!(settings_window.title().to_string(), "General");
        assert_eq!(
            settings.sidebar_titles(),
            ["Washboard", "General", "Customer API", "General", "Servers"],
            "the app's section, then one per open project"
        );

        // SAFETY: `projectSettings:` takes the sender.
        let _: () = unsafe { msg_send![&*project, projectSettings: None::<&AnyObject>] };
        assert_eq!(settings.selected(), Some(Pane::Servers(key)));
        assert_eq!(
            settings_window.title().to_string(),
            "Customer API — Servers"
        );
        let sheet = settings.servers(key);
        assert_settings_layout(&sheet);

        let names = table_column(sheet.table(), 0);
        assert_eq!(names, server_names(ctx, key));
        assert_eq!(names[1], "Production");
        assert!(sheet.suggestion_rows().is_empty());
        assert!(!sheet.shows_suggestions());

        sheet.table().click(1);
        assert_eq!(sheet.selected(), Some(1));
        let server = sheet.servers()[1].clone();
        assert_eq!(sheet.url_field().stringValue().to_string(), server.url);
        sheet
            .name_field()
            .setStringValue(&NSString::from_str("Production EU"));
        sheet.commit_form();
        assert_eq!(table_column(sheet.table(), 0)[1], "Production EU");
        assert_eq!(server_names(ctx, key)[1], "Production EU");

        assert_eq!(server.auth, Auth::None);
        assert!(
            !sheet.user_field().isEnabled(),
            "no user without Basic auth"
        );
        // SAFETY: `performClick:` takes any sender.
        unsafe { sheet.basic_auth_button().performClick(None) };
        assert!(
            sheet.user_field().isEnabled(),
            "Basic auth enables the user field"
        );
        sheet
            .user_field()
            .setStringValue(&NSString::from_str("bob"));
        sheet
            .password_field()
            .setStringValue(&NSString::from_str("hunter2"));
        sheet.commit_form();
        let basic = Auth::Basic {
            username: "bob".into(),
        };
        assert_eq!(sheet.servers()[1].auth, basic);
        // By project key, which changes when the project is reopened.
        let password = |ctx: &Ctx, key: ProjectKey| {
            ctx.delegate
                .read(|app| app.server_password(key, server.id))
                .and_then(Result::ok)
                .flatten()
        };
        assert_eq!(password(ctx, key).as_deref(), Some("hunter2"));
        assert_eq!(
            sheet.password_field().stringValue().to_string(),
            "",
            "a saved password leaves the field"
        );

        sheet.add_server();
        assert_eq!(server_names(ctx, key).len(), names.len() + 1);
        assert_eq!(sheet.selected(), Some(names.len()));
        sheet.remove_selected();
        assert_eq!(table_column(sheet.table(), 0), server_names(ctx, key));
        assert_eq!(server_names(ctx, key).len(), names.len());

        // A field being edited is saved when the window closes; no Done button.
        sheet.table().click(0);
        assert!(settings_window.makeFirstResponder(Some(sheet.name_field())));
        let editor = sheet
            .name_field()
            .currentEditor()
            .expect("editing the name");
        editor.setString(&NSString::from_str("Staging EU"));
        autoreleasepool(|_| settings_window.performClose(None));
        assert!(!settings_window.isVisible());
        assert_eq!(server_names(ctx, key)[0], "Staging EU");

        // The project's General pane, then the remembered pane on the next Settings….
        settings.select(Pane::ProjectGeneral(key));
        assert_eq!(
            settings_window.title().to_string(),
            "Customer API — General"
        );
        settings.select(Pane::Servers(key));
        // SAFETY: `showSettings:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, showSettings: None::<&AnyObject>] };
        assert_eq!(settings.selected(), Some(Pane::Servers(key)), "remembered");

        // Closing the project removes its section, and its pane gives way to the app's.
        autoreleasepool(|_| window.performClose(None));
        assert!(ctx.delegate.project(key).is_none(), "closed");
        assert_eq!(settings.sidebar_titles(), ["Washboard", "General"]);
        assert_eq!(settings.selected(), Some(Pane::App));
        autoreleasepool(|_| settings_window.performClose(None));

        let project = ctx.open();
        let window = project.project_window();
        let sheet = project.show_settings().expect("the Servers pane");
        assert_eq!(table_column(sheet.table(), 0)[1], "Production EU");
        sheet.select(1);
        assert_eq!(sheet.servers()[1].auth, basic);
        assert_eq!(sheet.user_field().stringValue().to_string(), "bob");
        assert_eq!(password(ctx, project.key()).as_deref(), Some("hunter2"));
        // Put the first server's name back for later checks.
        sheet.select(0);
        sheet
            .name_field()
            .setStringValue(&NSString::from_str("Staging"));
        sheet.commit_form();
        autoreleasepool(|_| settings_window.performClose(None));
        autoreleasepool(|_| window.performClose(None));
    }
}
