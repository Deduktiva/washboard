//! Application lifecycle: the `NSApplication` delegate and launching.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

use crate::menu;
use crate::welcome::{WelcomeController, sample_recent_projects};

#[derive(Debug, Default)]
pub struct AppDelegateIvars {
    welcome: OnceCell<Retained<WelcomeController>>,
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
            self.welcome().show();
            NSApplication::sharedApplication(self.mtm()).activate();
        }

        // SAFETY: the signature matches `applicationShouldTerminateAfterLastWindowClosed:`.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            // The welcome window takes over when the last project window closes.
            false
        }

        // SAFETY: the signature matches `applicationShouldHandleReopen:hasVisibleWindows:`.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            // Clicking the Dock icon with nothing open brings the welcome window back.
            if !has_visible_windows {
                self.welcome().show();
            }
            true
        }
    }

    // Menu actions that reach the app delegate through the responder chain. Stubs until
    // WP-APP-INTEGRATION binds them to `washboard-ui-model`.
    impl AppDelegate {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(newProject:))]
        fn new_project(&self, _sender: Option<&AnyObject>) {
            not_implemented("New Project");
        }

        #[unsafe(method(openProject:))]
        fn open_project(&self, _sender: Option<&AnyObject>) {
            not_implemented("Open Project");
        }

        #[unsafe(method(saveAll:))]
        fn save_all(&self, _sender: Option<&AnyObject>) {
            not_implemented("Save All");
        }

        #[unsafe(method(showHttpLog:))]
        fn show_http_log(&self, _sender: Option<&AnyObject>) {
            not_implemented("HTTP Log");
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    /// The welcome window's controller, created on first use.
    pub fn welcome(&self) -> &WelcomeController {
        self.ivars()
            .welcome
            .get_or_init(|| WelcomeController::new(sample_recent_projects(), self.mtm()))
    }
}

fn not_implemented(what: &str) {
    eprintln!("washboard-app: {what} is not implemented yet");
}

/// Creates the shared application, its main menu and a new delegate. `NSApplication` holds its
/// delegate weakly, so the caller must keep the returned delegate alive for as long as the app
/// runs.
pub fn install(mtm: MainThreadMarker) -> Retained<AppDelegate> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    menu::install(&app, mtm);
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
