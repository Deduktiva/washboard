//! Application lifecycle: the `NSApplication` delegate and launching.

use std::cell::{OnceCell, RefCell};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

use crate::http_log::{HttpLog, sample_exchanges};
use crate::menu;
use crate::project_window::ProjectWindowController;
use crate::sheets::{NewProjectSheet, sample_references};
use crate::welcome::{WelcomeController, sample_recent_projects};

#[derive(Debug, Default)]
pub struct AppDelegateIvars {
    welcome: OnceCell<Retained<WelcomeController>>,
    projects: RefCell<Vec<Retained<ProjectWindowController>>>,
    http_log: OnceCell<Retained<HttpLog>>,
    new_project: RefCell<Option<Retained<NewProjectSheet>>>,
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
            self.show_new_project_sheet();
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
            self.http_log().show();
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    /// Opens a project window (sample content for now) and hides the welcome window, which
    /// is shown only while no project is open.
    pub fn open_project_window(&self, name: &str) -> Retained<ProjectWindowController> {
        let project = ProjectWindowController::new(name, self.mtm());
        // SAFETY: `showWindow:` takes any sender.
        unsafe { project.showWindow(None) };
        self.ivars().projects.borrow_mut().push(project.clone());
        self.welcome().window().orderOut(None);
        project
    }

    /// Shows the New Project sheet on the key window, or on the welcome window when no window
    /// is key. A sheet already showing is brought forward rather than doubled.
    pub fn show_new_project_sheet(&self) -> Retained<NewProjectSheet> {
        let mtm = self.mtm();
        if let Some(sheet) = &*self.ivars().new_project.borrow()
            && sheet.window().sheetParent().is_some()
        {
            sheet.window().makeKeyAndOrderFront(None);
            return sheet.clone();
        }
        let sheet = NewProjectSheet::new(sample_references(), mtm);
        let weak = Weak::from(self);
        sheet.on_create(move |name| {
            if let Some(app) = weak.load() {
                app.open_project_window(name);
            }
        });
        let parent = match NSApplication::sharedApplication(mtm).keyWindow() {
            Some(window) if window.attachedSheet().is_none() => window,
            _ => {
                self.welcome().show();
                self.welcome().window().retain()
            }
        };
        sheet.present(&parent);
        *self.ivars().new_project.borrow_mut() = Some(sheet.clone());
        sheet
    }

    /// The most recent New Project sheet, if one was shown.
    pub fn new_project_sheet(&self) -> Option<Retained<NewProjectSheet>> {
        self.ivars().new_project.borrow().clone()
    }

    /// The open project windows' controllers, in opening order.
    pub fn projects(&self) -> Vec<Retained<ProjectWindowController>> {
        self.ivars().projects.borrow().clone()
    }

    /// Called from the project window's `windowWillClose:`.
    pub(crate) fn project_closed(&self, project: &ProjectWindowController) {
        // Keep the controller alive until AppKit is done closing its window; the run loop's
        // autorelease pool releases it afterwards.
        let closed: Vec<_> = {
            let mut projects = self.ivars().projects.borrow_mut();
            let (closed, open) = projects
                .drain(..)
                .partition(|p| std::ptr::eq(&**p, project));
            *projects = open;
            closed
        };
        for project in closed {
            let _ = Retained::autorelease_ptr(project);
        }
        if self.ivars().projects.borrow().is_empty() {
            self.welcome().show();
        }
    }

    /// The app's one HTTP log panel, created on first use.
    pub fn http_log(&self) -> &HttpLog {
        self.ivars()
            .http_log
            .get_or_init(|| HttpLog::new(sample_exchanges(), self.mtm()))
    }

    /// The welcome window's controller, created on first use.
    pub fn welcome(&self) -> &WelcomeController {
        self.ivars()
            .welcome
            .get_or_init(|| WelcomeController::new(sample_recent_projects(), self.mtm()))
    }
}

/// Runs `f` with the app delegate, if it is ours (it is, unless a test installed another).
pub(crate) fn with_delegate(mtm: MainThreadMarker, f: impl FnOnce(&AppDelegate)) {
    if let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() {
        let delegate: &AnyObject = delegate.as_ref();
        if let Some(delegate) = delegate.downcast_ref::<AppDelegate>() {
            f(delegate);
        }
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
