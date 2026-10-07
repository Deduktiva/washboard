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
    let checks: &[Check] = &[("lifecycle", checks::lifecycle)];
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
        NSEventType,
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

    /// Launching opens the main window, and closing the last window does not quit.
    pub fn lifecycle(ctx: &Ctx) {
        let window = ctx.delegate.main_window().expect("a window after launch");
        assert!(window.isVisible(), "the window is shown");

        // Ask through the Objective-C runtime, as AppKit does, so this also checks the
        // selector is registered.
        // SAFETY: the selector takes an `NSApplication` and returns `BOOL`.
        let terminate: bool = unsafe {
            msg_send![&*ctx.delegate, applicationShouldTerminateAfterLastWindowClosed: &*ctx.app]
        };
        assert!(!terminate, "closing the last window must not quit");
    }
}
