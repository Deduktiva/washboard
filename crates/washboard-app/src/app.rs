//! Application lifecycle: the `NSApplication` delegate and launching.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string,
};

#[derive(Debug, Default)]
pub struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `AppDelegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    #[derive(Debug)]
    pub struct AppDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: `NSApplicationDelegate` has no safety requirements.
    unsafe impl NSApplicationDelegate for AppDelegate {
        // SAFETY: the signature matches `applicationDidFinishLaunching:`.
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let window = placeholder_window(self.mtm());
            window.makeKeyAndOrderFront(None);
            // Set once: AppKit sends this notification once per launch.
            let _ = self.ivars().window.set(window);
            NSApplication::sharedApplication(self.mtm()).activate();
        }

        // SAFETY: the signature matches `applicationShouldTerminateAfterLastWindowClosed:`.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            // The welcome window takes over when the last project window closes.
            false
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    /// The window opened at launch; `None` before `applicationDidFinishLaunching:`.
    pub fn main_window(&self) -> Option<&NSWindow> {
        self.ivars().window.get().map(|w| &**w)
    }
}

/// Creates the shared application and installs a new delegate. `NSApplication` holds its
/// delegate weakly, so the caller must keep the returned delegate alive for as long as the app
/// runs.
pub fn install(mtm: MainThreadMarker) -> Retained<AppDelegate> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    delegate
}

/// Runs the app until it terminates.
pub fn run(mtm: MainThreadMarker) {
    let delegate = install(mtm);
    NSApplication::sharedApplication(mtm).run();
    drop(delegate);
}

fn placeholder_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(1000.0, 640.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
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
    // SAFETY: the delegate keeps the `Retained<NSWindow>`, so AppKit must not release it on
    // close as well.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Washboard"));
    window.center();
    window
}
