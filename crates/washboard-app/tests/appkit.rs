//! Headless AppKit checks, run by the macOS CI job (WP-APP-SHELL in `docs/TASKS.md`).
//!
//! `harness = false`: AppKit objects must be created on the main thread, and libtest runs
//! tests on worker threads. Each check builds the real objects from `washboard_app` without
//! starting the run loop and asserts on them; a failed assertion panics and fails the run.

#[cfg(target_os = "macos")]
type Check = (&'static str, fn(objc2::MainThreadMarker));

#[cfg(target_os = "macos")]
fn main() {
    let mtm = objc2::MainThreadMarker::new().expect("harness = false runs main on the main thread");
    let checks: &[Check] = &[("lifecycle", checks::lifecycle)];
    for (name, check) in checks {
        print!("appkit check {name} ... ");
        check(mtm);
        println!("ok");
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("appkit checks run on macOS only");
}

#[cfg(target_os = "macos")]
mod checks {
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::NSApplication;

    /// Launching opens the main window, and closing it does not quit the app.
    pub fn lifecycle(mtm: MainThreadMarker) {
        let delegate = washboard_app::install(mtm);
        let app = NSApplication::sharedApplication(mtm);
        // Posts `NSApplicationDidFinishLaunchingNotification`, as `run` would.
        app.finishLaunching();

        let window = delegate.main_window().expect("a window after launch");
        assert!(window.isVisible(), "the window is shown");

        // Ask through the Objective-C runtime, as AppKit does, so this also checks the
        // selector is registered.
        // SAFETY: the selector takes an `NSApplication` and returns `BOOL`.
        let terminate: bool = unsafe {
            msg_send![&*delegate, applicationShouldTerminateAfterLastWindowClosed: &*app]
        };
        assert!(!terminate, "closing the last window must not quit");
    }
}
