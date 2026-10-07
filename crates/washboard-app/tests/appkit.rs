//! Headless AppKit checks, run by the macOS CI job (WP-APP-SHELL in `docs/TASKS.md`).
//!
//! `harness = false`: AppKit objects must be created on the main thread, and libtest runs
//! tests on worker threads. The app is launched once, through `NSApplication::run` until
//! `applicationDidFinishLaunching:` has been delivered; then each check inspects the real
//! objects from `washboard_app` and asserts on them. A failed assertion panics and fails the
//! run. Checks share the application and run in order.

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
    use std::ptr::NonNull;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationDidFinishLaunchingNotification, NSEvent, NSEventModifierFlags,
        NSEventType, NSMenu, NSStackView, NSTextField, NSView,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSPoint};
    use washboard_app::AppDelegate;

    /// How long launching may take before the run is abandoned instead of hanging CI.
    const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

    pub struct Ctx {
        pub app: Retained<NSApplication>,
        pub delegate: Retained<AppDelegate>,
    }

    impl Ctx {
        /// `applicationDidFinishLaunching:` is sent from inside `run` (`finishLaunching` alone
        /// does not send it), so run the app and stop it once that notification has been
        /// delivered. The test's observer is added after the delegate's, so it runs second.
        pub fn launch(mtm: MainThreadMarker) -> Self {
            let delegate = washboard_app::install(mtm);
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
            Self { app, delegate }
        }
    }

    /// Launching shows the welcome window, and closing the last window does not quit.
    pub fn lifecycle(ctx: &Ctx) {
        let welcome = ctx.delegate.welcome().window();
        assert!(welcome.isVisible(), "welcome window after launch");

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
    pub fn main_menu(ctx: &Ctx) {
        let main = ctx.app.mainMenu().expect("a main menu");
        let items = main.itemArray();
        assert_eq!(items.len(), MENUS.len(), "top-level menus");
        for (item, (title, expected)) in items.iter().zip(MENUS) {
            let submenu = item.submenu().expect("top-level items have submenus");
            assert_eq!(submenu.title().to_string(), *title);
            let expected: Vec<_> = expected
                .iter()
                .map(|(t, k, m, a)| (t.to_string(), k.to_string(), *m, a.to_string()))
                .collect();
            assert_eq!(rows(&submenu), expected, "{title} menu");
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

    /// The recent-projects table shows the sample rows through its data source and delegate.
    pub fn welcome_window(ctx: &Ctx) {
        let welcome = ctx.delegate.welcome();
        let table = welcome.table();
        let sample = washboard_app::sample_recent_projects();
        assert_eq!(table.numberOfRows() as usize, sample.len());

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
        assert_eq!(labels, [sample[0].name.as_str(), sample[0].path.as_str()]);
    }
}
