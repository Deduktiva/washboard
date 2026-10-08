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
        ("sidebar_commands", checks::sidebar_commands),
        ("editor", checks::editor),
        ("editor_1mb_layout", checks::editor_1mb_layout),
        ("diagnostics", checks::diagnostics),
        ("fake_send", checks::fake_send),
        ("http_log", checks::http_log),
        ("new_project_sheet", checks::new_project_sheet),
        ("settings_sheet", checks::settings_sheet),
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
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{MainThreadMarker, Message, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationDidFinishLaunchingNotification, NSColor, NSEvent,
        NSEventModifierFlags, NSEventType, NSForegroundColorAttributeName, NSMenu,
        NSSplitViewItemBehavior, NSStackView, NSTextField, NSTextInputClient, NSView, NSWindow,
    };
    use objc2_foundation::{
        NSDate, NSIndexSet, NSNotification, NSNotificationCenter, NSObjectProtocol, NSPoint,
        NSRange, NSRunLoop, NSString,
    };
    use tempfile::TempDir;
    use washboard_app::{
        AppDelegate, EditorController, NodeKind, Options, ProjectWindowController, SidebarNode,
    };
    use washboard_core::model::{Auth, Server, ServerId};
    use washboard_core::project::{AppState, OpenProject, Project, WsdlFile, WsdlSet};
    use washboard_core::secrets::MemorySecretStore;
    use washboard_ui_model::{Alert, Confirm, DialogAnswer, DialogId, Dialogs};

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
            let options = Options {
                state_dir,
                secrets: Arc::new(MemorySecretStore::default()),
                dialogs: Some(Box::new(dialogs)),
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

    /// (title, key equivalent, modifiers, action); `-` is a separator, an action of `>` a
    /// submenu.
    type Row = (&'static str, &'static str, usize, &'static str);

    const MENUS: &[(&str, &[Row])] = &[
        (
            "Washboard",
            &[
                ("About Washboard", "", 0, "orderFrontStandardAboutPanel:"),
                ("-", "", 0, ""),
                ("Settings…", ",", C, ""),
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
            ],
        ),
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
                ("-", "", 0, ""),
                ("Replace WSDL…", "", 0, "replaceWsdl:"),
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

        // Settings has no action yet, so AppKit disables it.
        let app_menu = items
            .firstObject()
            .and_then(|i| i.submenu())
            .expect("app menu");
        let settings = app_menu.itemWithTitle(&objc2_foundation::NSString::from_str("Settings…"));
        app_menu.update();
        assert!(
            !settings.expect("Settings item").isEnabled(),
            "Settings is disabled"
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

        let table = welcome.table();
        assert_eq!(table.numberOfRows(), 1);
        let view = table
            .viewAtColumn_row_makeIfNecessary(0, 0, true)
            .expect("row 0 view");
        let stack = view
            .downcast::<NSStackView>()
            .expect("rows are stack views");
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

        welcome.open_recent(0);
        assert_eq!(ctx.delegate.projects().len(), 1, "reopened");
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
        autoreleasepool(|_| projects[1].project_window().performClose(None));
        assert_eq!(ctx.delegate.projects().len(), 1);
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

        // Operations are listed, unsupported ones too, and can be selected.
        let nodes = all_nodes(&project.sidebar().roots());
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
                .downcast::<NSStackView>()
                .expect("request rows are stacks")
                .arrangedSubviews()
                .firstObject()
                .and_then(|v| v.downcast::<NSTextField>().ok())
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

    /// The file of the request selected in `project`.
    fn selected_file(ctx: &Ctx, project: &ProjectWindowController) -> PathBuf {
        let id = project.selected_request().expect("a selected request");
        let name = all_nodes(&project.sidebar().roots())
            .into_iter()
            .find(|n| n.request() == Some(id))
            .expect("the selected request's row")
            .title();
        ctx.project.join("requests").join(format!("{name}.xml"))
    }

    /// The (unsaved, invalid) markers of the selected request's row.
    fn selected_markers(project: &ProjectWindowController) -> (bool, bool) {
        let id = project.selected_request().expect("a selected request");
        all_nodes(&project.sidebar().roots())
            .into_iter()
            .find(|n| n.request() == Some(id))
            .expect("the selected request's row")
            .markers()
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

        let replace = |at: usize, len: usize, with: &str| {
            // SAFETY: replacing a range inside the text, as typing over a selection does.
            unsafe {
                text_view
                    .insertText_replacementRange(&NSString::from_str(with), NSRange::new(at, len))
            };
        };
        replace(start, date.len(), "someday");
        wait_until("the invalid date to be listed", || {
            !issues.rows().is_empty()
        });
        assert_eq!(issues.rows()[0][0], line.to_string(), "{:?}", issues.rows());
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

        replace(start, "someday".len(), &date);
        let validate = std::ffi::CString::new("validateRequest:").expect("a selector name");
        // SAFETY: the window controller's actions take the sender.
        let sent = unsafe {
            ctx.app
                .sendAction_to_from(Sel::register(&validate), Some(&project), None)
        };
        assert!(sent);
        wait_until("the issues to clear", || issues.rows().is_empty());
        assert!(editor.ruler().error_lines().is_empty(), "gutter cleared");
        wait_until("the marker to clear", || !selected_markers(&project).1);
        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// A fake send finishes on the main thread and fills the response pane.
    pub fn fake_send(ctx: &Ctx) {
        let project = ctx.open();

        let response = project.response();
        let tabs: Vec<String> = response
            .tabs()
            .tabViewItems()
            .iter()
            .map(|t| t.label().to_string())
            .collect();
        assert_eq!(tabs, washboard_app::RESPONSE_TABS);

        project.fake_send();
        assert_eq!(response.status(), "Sending…");
        // The result arrives through the main dispatch queue, which the run loop drains.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !response.status().starts_with("200") {
            assert!(
                Instant::now() < deadline,
                "fake send did not finish: {}",
                response.status()
            );
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
        assert_eq!(response.history().rows().len(), 1, "history row added");
        let body = response.body().text_view().string().to_string();
        assert!(body.contains("GetCustomerResponse"), "response body shown");

        autoreleasepool(|_| project.project_window().performClose(None));
    }

    /// One log panel for the app, opened from the menu action, with `Authorization` masked
    /// until revealed.
    pub fn http_log(ctx: &Ctx) {
        // SAFETY: `showHttpLog:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, showHttpLog: None::<&AnyObject>] };
        let log = ctx.delegate.http_log();
        assert!(log.panel().isVisible(), "log panel shown");
        assert_eq!(
            log.table().rows().len(),
            washboard_app::sample_exchanges().len()
        );

        let secret = "YWxpY2U6c2VjcmV0";
        assert!(
            log.request_text().contains("Authorization: ••••••••"),
            "masked"
        );
        assert!(!log.request_text().contains(secret), "secret hidden");
        log.set_revealed(true);
        assert!(log.request_text().contains(secret), "revealed on request");
        log.set_revealed(false);
        assert!(!log.request_text().contains(secret), "masked again");
        log.panel().close();
    }

    /// Pumps the main run loop until `done` holds; sheets attach and detach asynchronously.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
    }

    /// Create stays disabled while a reference is unresolved, then ends the sheet. Creating the
    /// project is step 6 of WP-APP-INTEGRATION.
    pub fn new_project_sheet(ctx: &Ctx) {
        // SAFETY: `newProject:` takes the sender.
        let _: () = unsafe { msg_send![&*ctx.delegate, newProject: None::<&AnyObject>] };
        let welcome = ctx.delegate.welcome().window().retain();
        wait_until("the sheet to attach", || welcome.attachedSheet().is_some());
        let sheet = ctx
            .delegate
            .new_project_sheet()
            .expect("created by newProject:");
        assert_eq!(
            welcome
                .attachedSheet()
                .as_deref()
                .map(|w| w as *const NSWindow),
            Some(sheet.window() as *const NSWindow),
            "one New Project sheet, on the welcome window"
        );

        let marks: Vec<String> = sheet
            .table()
            .rows()
            .into_iter()
            .map(|r| r[0].clone())
            .collect();
        assert_eq!(marks, ["✓", "✓", "✗"]);
        assert!(!sheet.create_button().isEnabled(), "disabled while ✗");
        sheet.create();
        assert!(
            ctx.delegate.projects().is_empty(),
            "disabled Create does nothing"
        );

        let all_resolved = washboard_app::sample_references()
            .into_iter()
            .map(|mut r| {
                r.resolved_to.get_or_insert_with(|| "addresses.xsd".into());
                r
            })
            .collect();
        sheet.set_references(all_resolved);
        assert!(sheet.create_button().isEnabled(), "enabled once all ✓");

        sheet
            .name_field()
            .setStringValue(&NSString::from_str("Billing"));
        sheet.create();
        wait_until("the sheet to end", || welcome.attachedSheet().is_none());
    }

    /// Project Settings opens on Servers; the form edits the selected row, auth enables the
    /// user field, and + / − add and remove rows.
    pub fn settings_sheet(ctx: &Ctx) {
        let project = ctx.open();
        let window = project.project_window();
        // SAFETY: `projectSettings:` takes the sender.
        let _: () = unsafe { msg_send![&*project, projectSettings: None::<&AnyObject>] };
        wait_until("the sheet to attach", || window.attachedSheet().is_some());
        let sheet = project.settings().expect("created by projectSettings:");
        let selected_tab = sheet
            .tabs()
            .selectedTabViewItem()
            .map(|t| t.label().to_string());
        assert_eq!(selected_tab.as_deref(), Some("Servers"));

        let names = |sheet: &washboard_app::SettingsSheet| -> Vec<String> {
            sheet
                .table()
                .rows()
                .into_iter()
                .map(|r| r[0].clone())
                .collect()
        };
        assert_eq!(names(sheet), ["Production", "Staging", "Local"]);

        sheet.table().click(1);
        assert_eq!(sheet.selected(), Some(1));
        assert_eq!(
            sheet.url_field().stringValue().to_string(),
            "https://stg.example.com/ws/customer"
        );
        sheet
            .name_field()
            .setStringValue(&NSString::from_str("Staging EU"));
        sheet.commit_form();
        assert_eq!(names(sheet), ["Production", "Staging EU", "Local"]);

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
        assert!(sheet.servers()[1].basic_auth);

        sheet.add_server();
        assert_eq!(sheet.servers().len(), 4);
        assert_eq!(sheet.selected(), Some(3));
        sheet.remove_selected();
        assert_eq!(names(sheet), ["Production", "Staging EU", "Local"]);

        // SAFETY: `done:` takes the sender.
        let _: () = unsafe { msg_send![sheet, done: None::<&AnyObject>] };
        wait_until("the sheet to end", || window.attachedSheet().is_none());
        autoreleasepool(|_| window.performClose(None));
    }
}
