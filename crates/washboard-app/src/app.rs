//! Application lifecycle: the `NSApplication` delegate, which owns the model.
//!
//! Every input reaches `washboard-ui-model` through [`AppDelegate::command`] or
//! [`AppDelegate::update`]: borrow the model, run, release, then [`AppDelegate::sync`] applies
//! the events the model queued. The model is never borrowed while AppKit runs, so a view
//! callback triggered by applying an event can issue its own command.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSMenuItem,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString, NSUserDefaults};
use washboard_core::secrets::SecretStore;
use washboard_ui_model::{
    App, DialogAnswer, DialogId, Dialogs, Event, FormatSettings, FrontEnd, ImportTarget,
    ModelError, ProjectKey, TimerId,
};

use crate::app_settings;
use crate::front_end::{AppKitDialogs, DispatchTimers, Wake, default_state_dir, delegate_ref};
use crate::http_log::HttpLog;
use crate::menu;
use crate::project_window::ProjectWindowController;
use crate::settings_window::{Pane, SettingsWindowController};
use crate::sheets::ImportSheetController;
use crate::welcome::{RecentProject, WelcomeController};

/// What the app runs with. Tests pass a temp state dir, an in-memory secret store and dialogs
/// that record instead of showing panels.
pub struct Options {
    /// Holds `state.json`.
    pub state_dir: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    /// `None`: AppKit open panels and alerts.
    pub dialogs: Option<Box<dyn Dialogs>>,
    /// The user defaults suite for the app's settings; `None`: the app's own defaults
    /// (`at.deduktiva.washboard` when run from the bundle).
    pub defaults_suite: Option<String>,
}

impl Options {
    /// `~/Library/Application Support/Washboard`, the Keychain, AppKit panels.
    pub fn standard() -> Options {
        Options {
            state_dir: default_state_dir(),
            secrets: Arc::new(washboard_core::secrets::KeychainSecretStore),
            dialogs: None,
            defaults_suite: None,
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub struct AppDelegateIvars {
    /// Set right after the delegate is created: the front end it gets needs the delegate.
    model: RefCell<Option<App>>,
    /// The generation of each running timer (see `DispatchTimers`).
    timers: Rc<RefCell<HashMap<TimerId, u64>>>,
    welcome: OnceCell<Retained<WelcomeController>>,
    projects: RefCell<Vec<Retained<ProjectWindowController>>>,
    http_log: OnceCell<Retained<HttpLog>>,
    new_project: RefCell<Option<Retained<ImportSheetController>>>,
    defaults: OnceCell<Retained<NSUserDefaults>>,
    settings: OnceCell<Retained<SettingsWindowController>>,
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
            self.update(App::launch);
            self.sync();
            NSApplication::sharedApplication(self.mtm()).activate();
        }

        // SAFETY: the signature matches `applicationShouldTerminateAfterLastWindowClosed:`.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            // The welcome window takes over when the last project window closes.
            false
        }

        // SAFETY: the signature matches `applicationShouldTerminate:`.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            let quit = self.update(App::quit);
            self.sync();
            match quit {
                // An editor could not be saved; the model has said why.
                Some(Ok(false)) => NSApplicationTerminateReply::TerminateCancel,
                // Every edit is saved; only the list of open projects is lost.
                Some(Err(e)) => {
                    eprintln!("washboard-app: could not save the app state: {e}");
                    NSApplicationTerminateReply::TerminateNow
                }
                Some(Ok(true)) | None => NSApplicationTerminateReply::TerminateNow,
            }
        }

        // SAFETY: the signature matches `applicationDidResignActive:`.
        #[unsafe(method(applicationDidResignActive:))]
        fn did_resign_active(&self, _notification: &NSNotification) {
            // Switching apps saves every edit (PLAN §4 "Save / autosave").
            self.update(App::app_deactivated);
            self.sync();
        }

        // SAFETY: the signature matches `applicationShouldHandleReopen:hasVisibleWindows:`.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            // Clicking the Dock icon with nothing open brings the welcome window back.
            if !has_visible_windows && self.read(App::welcome_visible).unwrap_or(true) {
                self.welcome().show();
            }
            true
        }

        // SAFETY: the signature matches `application:openFile:`.
        #[unsafe(method(application:openFile:))]
        fn open_file(&self, _sender: &NSApplication, filename: &NSString) -> bool {
            self.open_project_at(Path::new(&filename.to_string())).is_some()
        }
    }

    // Menu actions that reach the app delegate through the responder chain.
    impl AppDelegate {
        // SAFETY (all below): action methods take the sender and return nothing.
        #[unsafe(method(newProject:))]
        fn new_project(&self, _sender: Option<&AnyObject>) {
            self.show_new_project_sheet();
        }

        #[unsafe(method(openProject:))]
        fn open_project(&self, _sender: Option<&AnyObject>) {
            self.update(App::choose_and_open_project);
            self.sync();
        }

        #[unsafe(method(openRecentProject:))]
        fn open_recent_project(&self, sender: Option<&AnyObject>) {
            let index = sender
                .and_then(|s| s.downcast_ref::<NSMenuItem>())
                .and_then(|item| usize::try_from(item.tag()).ok());
            if let Some(index) = index {
                self.open_recent(index);
            }
        }

        #[unsafe(method(clearRecentProjects:))]
        fn clear_recent_projects(&self, _sender: Option<&AnyObject>) {
            self.update(App::clear_recent_projects);
            self.sync();
        }

        #[unsafe(method(saveAll:))]
        fn save_all(&self, _sender: Option<&AnyObject>) {
            self.save_all_requests();
        }

        #[unsafe(method(showSettings:))]
        fn show_settings_action(&self, _sender: Option<&AnyObject>) {
            self.show_settings(None);
        }

        #[unsafe(method(showHttpLog:))]
        fn show_http_log(&self, _sender: Option<&AnyObject>) {
            self.http_log().show();
        }
    }
);

impl AppDelegate {
    fn new(options: Options, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        // SAFETY: `NSObject`'s `init` has this signature.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let delegate = delegate_ref(&this, mtm);
        let dialogs = options
            .dialogs
            .unwrap_or_else(|| Box::new(AppKitDialogs::new(delegate.clone())));
        let front = FrontEnd {
            main_thread: Arc::new(Wake::new(delegate.clone())),
            timers: Box::new(DispatchTimers::new(delegate, this.ivars().timers.clone())),
            dialogs,
            secrets: options.secrets,
        };
        let defaults = options
            .defaults_suite
            .and_then(|suite| {
                NSUserDefaults::initWithSuiteName(
                    NSUserDefaults::alloc(),
                    Some(&NSString::from_str(&suite)),
                )
            })
            .unwrap_or_else(NSUserDefaults::standardUserDefaults);
        let mut model = App::new(options.state_dir, front);
        model.set_format_settings(app_settings::load(&defaults));
        *this.ivars().model.borrow_mut() = Some(model);
        let _ = this.ivars().defaults.set(defaults);
        this
    }

    /// The user defaults the app's settings live in.
    pub fn defaults(&self) -> &NSUserDefaults {
        self.ivars().defaults.get().expect("set in new()")
    }

    /// The settings window changed a setting: the model uses it from now on, the defaults keep
    /// it for the next launch.
    pub fn set_format_settings(&self, settings: FormatSettings) {
        self.update(|app| app.set_format_settings(settings));
        let stored = self.read(App::format_settings).unwrap_or(settings);
        app_settings::store(self.defaults(), stored);
    }

    /// The Settings window, created on first use.
    pub fn settings_window(&self) -> &SettingsWindowController {
        self.ivars()
            .settings
            .get_or_init(|| SettingsWindowController::new(self.mtm()))
    }

    /// Washboard ▸ Settings… (`None`: the pane last shown) and Project ▸ Project Settings….
    pub fn show_settings(&self, pane: Option<Pane>) -> &SettingsWindowController {
        let window = self.settings_window();
        window.show(pane);
        window
    }

    /// File ▸ Save All: with format on save, formats each project's edited request first (one
    /// undo step each, as Format XML), then saves every edit.
    pub fn save_all_requests(&self) {
        for key in self.read(App::format_on_save).unwrap_or_default() {
            if let Some(controller) = self.project(key) {
                controller.format_request(false);
            }
        }
        self.update(App::save_all);
        self.sync();
    }

    /// Runs `f` on the model and alerts with `title` if it fails, then applies the events.
    pub fn command<R>(
        &self,
        title: &str,
        f: impl FnOnce(&mut App) -> Result<R, ModelError>,
    ) -> Option<R> {
        let result = self.update(|app| {
            let result = f(app);
            if let Err(e) = &result {
                app.alert_error(title, e);
            }
            result.ok()
        });
        self.sync();
        result.flatten()
    }

    /// Runs `f` on the model. The caller applies the events with [`sync`](Self::sync).
    pub fn update<R>(&self, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let mut model = self.ivars().model.borrow_mut();
        model.as_mut().map(f)
    }

    /// Reads from the model.
    pub fn read<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        let model = self.ivars().model.borrow();
        model.as_ref().map(f)
    }

    /// Applies the events the model has queued, until it queues no more.
    pub fn sync(&self) {
        loop {
            let events = self.update(App::take_events).unwrap_or_default();
            if events.is_empty() {
                return;
            }
            for event in events {
                self.apply(event);
            }
        }
    }

    /// Worker results are waiting: apply them. Called from the main queue after a wake.
    pub fn pump(&self) {
        self.update(App::pump);
        self.sync();
    }

    /// A timer block ran; it counts only if the timer was not restarted or cancelled since.
    pub(crate) fn timer_due(&self, id: TimerId, generation: u64) {
        let current = {
            let mut timers = self.ivars().timers.borrow_mut();
            let current = timers.get(&id) == Some(&generation);
            if current {
                timers.remove(&id);
            }
            current
        };
        if current {
            self.update(|app| app.timer_fired(id));
            self.sync();
        }
    }

    /// The front end's answer to a dialog the model asked for.
    pub fn dialog_answered(&self, id: DialogId, answer: DialogAnswer) {
        self.update(|app| app.dialog_answered(id, answer));
        self.sync();
    }

    /// Opens the project in `folder`, or brings its window forward.
    pub fn open_project_at(&self, folder: &Path) -> Option<ProjectKey> {
        self.command(&format!("Could not open {}", folder.display()), |app| {
            app.open_project(folder)
        })
    }

    /// Opens the `index`th recent project.
    pub fn open_recent(&self, index: usize) -> Option<ProjectKey> {
        let folder = self.read(|app| app.recent_projects().get(index).cloned())??;
        self.open_project_at(&folder)
    }

    /// The window controller of an open project.
    pub fn project(&self, key: ProjectKey) -> Option<Retained<ProjectWindowController>> {
        self.ivars()
            .projects
            .borrow()
            .iter()
            .find(|p| p.key() == key)
            .cloned()
    }

    /// The open project windows' controllers, in opening order.
    pub fn projects(&self) -> Vec<Retained<ProjectWindowController>> {
        self.ivars().projects.borrow().clone()
    }

    /// Shows the New Project window. It is not a sheet, so open projects stay usable while it
    /// is up. One already showing is brought forward rather than doubled.
    pub fn show_new_project_sheet(&self) -> Retained<ImportSheetController> {
        let mtm = self.mtm();
        if let Some(sheet) = &*self.ivars().new_project.borrow()
            && sheet.window().isVisible()
        {
            sheet.window().makeKeyAndOrderFront(None);
            return sheet.clone();
        }
        let target = ImportTarget::NewProject;
        self.update(|app| app.begin_import(target));
        let sheet = ImportSheetController::new(target, mtm);
        sheet.show();
        *self.ivars().new_project.borrow_mut() = Some(sheet.clone());
        // Shows the model's sheet through `ImportChanged`.
        self.sync();
        sheet
    }

    /// The most recent New Project sheet, if one was shown.
    pub fn new_project_sheet(&self) -> Option<Retained<ImportSheetController>> {
        self.ivars().new_project.borrow().clone()
    }

    /// The window's close button: the model saves the editor first and keeps the project open
    /// if that fails. Returns whether the window may close.
    pub(crate) fn close_requested(&self, project: &ProjectWindowController) -> bool {
        project.set_closing(true);
        let closed = self
            .update(|app| app.close_project(project.key()))
            .unwrap_or(true);
        if !closed {
            project.set_closing(false);
        }
        self.sync();
        closed
    }

    /// The app's one HTTP log panel, created on first use.
    pub fn http_log(&self) -> &HttpLog {
        self.ivars().http_log.get_or_init(|| {
            let log = HttpLog::new(self.mtm());
            log.reload();
            log
        })
    }

    /// The welcome window's controller, created on first use.
    pub fn welcome(&self) -> &WelcomeController {
        self.ivars()
            .welcome
            .get_or_init(|| WelcomeController::new(self.mtm()))
    }

    fn apply(&self, event: Event) {
        match event {
            Event::ProjectOpened { project } => self.project_opened(project),
            Event::ProjectClosed { project } => self.project_closed(project),
            Event::FocusProject { project } => {
                if let Some(controller) = self.project(project) {
                    controller.project_window().makeKeyAndOrderFront(None);
                }
            }
            Event::WelcomeVisibility { visible: true } => self.welcome().show(),
            Event::WelcomeVisibility { visible: false } => {
                self.welcome().window().orderOut(None);
            }
            Event::RecentProjectsChanged => self.recent_projects_changed(),
            Event::SidebarChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.sidebar().reload();
                    // A rename changes the request bar's name.
                    controller.show_request_bar();
                }
            }
            Event::SelectionChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.sidebar().show_selection(None);
                }
            }
            Event::BeginRename { project, request } => {
                if let Some(controller) = self.project(project) {
                    controller.sidebar().begin_rename(request);
                }
            }
            Event::ServersChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.reload_servers();
                }
                if let Some(settings) = self.ivars().settings.get() {
                    settings.reload_servers(project);
                }
            }
            Event::ServerSelectionChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.show_server_selection();
                }
            }
            Event::EditorReplaced { project } => {
                if let Some(controller) = self.project(project) {
                    // The request bar, response pane and history follow the editor's request.
                    controller.editor().show_model_text();
                    controller.show_request_bar();
                    controller.response().show_response();
                    controller.response().show_history();
                }
            }
            Event::TokensChanged { project, range } => {
                if let Some(controller) = self.project(project) {
                    controller.editor().recolor(range);
                }
            }
            Event::DiagnosticsChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.show_issues();
                }
            }
            Event::ShowIssues { project } => {
                if let Some(controller) = self.project(project) {
                    controller.issues().reveal();
                }
            }
            Event::SendStateChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.show_send_state();
                }
            }
            Event::ResponseChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.response().show_response();
                }
            }
            Event::HistoryChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.response().show_history();
                }
            }
            Event::LogAppended => {
                if let Some(log) = self.ivars().http_log.get() {
                    log.reload();
                }
            }
            Event::EditedChanged { project } => {
                if let Some(controller) = self.project(project) {
                    controller.show_edited();
                }
            }
            Event::ImportChanged { target } => {
                let sheet = match target {
                    ImportTarget::NewProject => self.new_project_sheet(),
                    ImportTarget::ReplaceWsdl(key) => {
                        self.project(key).and_then(|c| c.replace_sheet())
                    }
                };
                if let Some(sheet) = sheet {
                    sheet.reload();
                }
            }
            Event::WsdlReplaced { project } => {
                if let Some(controller) = self.project(project) {
                    controller.show_replace_outcome();
                }
            }
        }
    }

    fn project_opened(&self, key: ProjectKey) {
        let Some((name, path)) = self
            .read(|app| {
                let window = app.project(key)?;
                Some((window.name().to_owned(), window.path().to_owned()))
            })
            .flatten()
        else {
            return;
        };
        let controller = ProjectWindowController::new(key, &name, &path, self.mtm());
        self.ivars().projects.borrow_mut().push(controller.clone());
        // The model announces a new project once; its state so far is read here.
        controller.sidebar().reload();
        controller.reload_servers();
        controller.editor().show_model_text();
        controller.show_request_bar();
        controller.show_issues();
        controller.response().show_response();
        controller.response().show_history();
        controller.show_edited();
        // SAFETY: `showWindow:` takes any sender.
        unsafe { controller.showWindow(None) };
        if let Some(settings) = self.ivars().settings.get() {
            settings.projects_changed();
        }
    }

    fn project_closed(&self, key: ProjectKey) {
        let closed: Vec<_> = {
            let mut projects = self.ivars().projects.borrow_mut();
            let (closed, open) = projects.drain(..).partition(|p| p.key() == key);
            *projects = open;
            closed
        };
        for controller in closed {
            if !controller.is_closing() {
                controller.set_closing(true);
                controller.project_window().close();
            }
            // Keep the controller alive until AppKit is done closing its window; the run
            // loop's autorelease pool releases it afterwards.
            let _ = Retained::autorelease_ptr(controller);
        }
        // The project's section leaves the Settings window.
        if let Some(settings) = self.ivars().settings.get() {
            settings.projects_changed();
        }
    }

    fn recent_projects_changed(&self) {
        let recent: Vec<RecentProject> = self
            .read(|app| {
                app.recent_projects()
                    .iter()
                    .map(|folder| RecentProject::new(folder))
                    .collect()
            })
            .unwrap_or_default();
        menu::set_recent_projects(&NSApplication::sharedApplication(self.mtm()), &recent);
        self.welcome().set_recent(recent);
    }
}

/// Runs `f` with the app delegate, if it is ours (it is, unless a test installed another).
/// The model as a controller sees it: through the app delegate, which owns it. `None` once the
/// delegate is gone (the app is terminating).
pub(crate) trait ModelAccess: MainThreadOnly {
    fn read<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        with_delegate(self.mtm(), |d| d.read(f)).flatten()
    }

    /// [`AppDelegate::command`]: alerts with `title` if `f` fails.
    fn command<R>(
        &self,
        title: &str,
        f: impl FnOnce(&mut App) -> Result<R, ModelError>,
    ) -> Option<R> {
        with_delegate(self.mtm(), |d| d.command(title, f)).flatten()
    }
}

pub(crate) fn with_delegate<R>(
    mtm: MainThreadMarker,
    f: impl FnOnce(&AppDelegate) -> R,
) -> Option<R> {
    let delegate = NSApplication::sharedApplication(mtm).delegate()?;
    let delegate: &AnyObject = delegate.as_ref();
    delegate.downcast_ref::<AppDelegate>().map(f)
}

/// Creates the shared application, its main menu and a new delegate with its model.
/// `NSApplication` holds its delegate weakly, so the caller must keep the returned delegate
/// alive for as long as the app runs.
pub fn install(options: Options, mtm: MainThreadMarker) -> Retained<AppDelegate> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    menu::install(&app, mtm);
    let delegate = AppDelegate::new(options, mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    delegate
}

/// Runs the app until it terminates.
pub fn run(mtm: MainThreadMarker) {
    let delegate = install(Options::standard(), mtm);
    NSApplication::sharedApplication(mtm).run();
    drop(delegate);
}
