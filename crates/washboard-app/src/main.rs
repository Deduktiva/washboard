//! Washboard macOS app (AppKit via objc2).
//!
//! Owned by WP-APP (`docs/TASKS.md`). Cloud agents run on Linux: type-check this crate with
//! `cargo check -p washboard-app --target aarch64-apple-darwin`; CI builds it on macOS.

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let mtm = MainThreadMarker::new().expect("main() runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(1000.0, 640.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: standard designated initializer, called on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: we keep the Retained<NSWindow> alive for the app's lifetime.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Washboard"));
    window.center();
    window.makeKeyAndOrderFront(None);
    app.activate();
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("washboard-app runs on macOS only");
    std::process::exit(1);
}
