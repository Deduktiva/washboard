//! App-level state: open projects, recent projects, restore on launch, and the plumbing that
//! brings worker results back to the main thread.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use thiserror::Error;
use washboard_core::model::{RequestId, ServerId};
use washboard_core::project::{AppState, AppStateError, OpenProject, Project, ProjectError};
use washboard_core::soap::EnvelopeError;

use crate::diagnostics::Check;
use crate::event::Event;
use crate::format::FormatSettings;
use crate::front_end::{Alert, DialogAnswer, DialogId, FrontEnd, TimerId};
use crate::import::Imports;
use crate::send::LogEntry;
use crate::timers::TimerKind;
use crate::window::{ProjectSchema, ProjectWindow, SchemaState, Sidebar, operation_tree};

#[derive(Debug, Error)]
pub enum ModelError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    AppState(#[from] AppStateError),
    /// The project was closed while the front end still referred to it.
    #[error("the project is no longer open")]
    UnknownProject,
    /// Commands that need the WSDL (New Request) wait until it has loaded.
    #[error("the WSDL is still loading")]
    SchemaNotReady,
    #[error("the WSDL could not be loaded: {0}")]
    SchemaFailed(String),
    /// Every operation in the WSDL is unsupported (SOAP 1.2, rpc/encoded, …).
    #[error("the WSDL has no operation Washboard can call")]
    NoSupportedOperation,
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    /// Send needs a server; the project has none.
    #[error("no server is configured")]
    NoServer,
    #[error("a request is already being sent")]
    AlreadySending,
    /// The import sheet was closed while the front end still referred to it.
    #[error("the import sheet is not open")]
    NoImport,
    /// Create / Replace while the check is running or found problems.
    #[error("the WSDL import is not ready")]
    ImportNotReady,
    /// Editing needs a selected request whose text could be read.
    #[error("no request is open in the editor")]
    NoRequestSelected,
    /// Format XML leaves a request that is not well-formed alone.
    #[error("the request is not well-formed XML: {0}")]
    NotWellFormed(String),
}

/// Identifies an open project for as long as it is open. Not reused within one run, so a
/// late event for a closed project can never reach a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectKey(u64);

/// What an outstanding dialog was asked for.
#[derive(Debug)]
pub(crate) enum PendingDialog {
    OpenProject,
    DeleteRequest {
        project: ProjectKey,
        request: RequestId,
    },
    DeleteServer {
        project: ProjectKey,
        server: ServerId,
    },
}

/// Work finished on a worker thread, to be applied on the main thread.
type Completion = Box<dyn FnOnce(&mut App) + Send>;

/// The whole model. Owned by the front end and only used on the main thread.
pub struct App {
    pub(crate) front: FrontEnd,
    state_dir: PathBuf,
    recent: Vec<PathBuf>,
    pub(crate) projects: Vec<(ProjectKey, ProjectWindow)>,
    pub(crate) events: Vec<Event>,
    welcome_visible: Option<bool>,
    pub(crate) dialogs: HashMap<DialogId, PendingDialog>,
    /// Running timers and what they are for.
    pub(crate) timers: HashMap<TimerId, (ProjectKey, TimerKind)>,
    next_id: u64,
    done_tx: Sender<Completion>,
    done_rx: Receiver<Completion>,
    jobs: usize,
    pub(crate) log: VecDeque<LogEntry>,
    pub(crate) imports: Imports,
    pub(crate) format: FormatSettings,
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("state_dir", &self.state_dir)
            .field("projects", &self.projects)
            .finish_non_exhaustive()
    }
}

impl App {
    /// `state_dir` holds `state.json`: `~/Library/Application Support/Washboard` on macOS.
    pub fn new(state_dir: impl Into<PathBuf>, front: FrontEnd) -> App {
        let (done_tx, done_rx) = channel();
        App {
            front,
            state_dir: state_dir.into(),
            recent: Vec::new(),
            projects: Vec::new(),
            events: Vec::new(),
            welcome_visible: None,
            dialogs: HashMap::new(),
            timers: HashMap::new(),
            next_id: 0,
            done_tx,
            done_rx,
            jobs: 0,
            log: VecDeque::new(),
            imports: Imports::new(),
            format: FormatSettings::default(),
        }
    }

    /// Reopens the projects that were open at quit (PLAN §4 "Multiple projects / restore").
    /// Projects that can't be opened are reported in one alert and dropped from the saved
    /// list right away, so they are reported once, not on every launch. An unreadable
    /// `state.json` starts empty, also with an alert.
    pub fn launch(&mut self) {
        let state = match AppState::load(&self.state_dir) {
            Ok(state) => state,
            Err(e) => {
                self.front.dialogs.alert(Alert {
                    title: "Washboard could not restore the last session".into(),
                    message: e.to_string(),
                });
                AppState::default()
            }
        };
        self.recent = state.recent_projects;
        self.events.push(Event::RecentProjectsChanged);

        let mut failed = Vec::new();
        for restore in state.open_projects {
            match Project::open(&restore.path) {
                Ok(project) => {
                    self.add_project(project, restore);
                }
                Err(e) => failed.push(format!("{}: {e}", restore.path.display())),
            }
        }
        if !failed.is_empty() {
            self.front.dialogs.alert(Alert {
                title: "Some projects could not be reopened".into(),
                message: failed.join("\n"),
            });
            if let Err(e) = self.save_state() {
                self.alert_error("Washboard could not save its state", &e);
            }
        }
        self.update_welcome();
    }

    /// Opens the project in `folder`, or brings its window forward if it is already open.
    pub fn open_project(&mut self, folder: &Path) -> Result<ProjectKey, ModelError> {
        if let Some(key) = self.key_for_path(folder) {
            self.events.push(Event::FocusProject { project: key });
            return Ok(key);
        }
        let project = Project::open(folder)?;
        let key = self.add_project(project, OpenProject::new(folder));
        self.note_recent(folder);
        self.update_welcome();
        Ok(key)
    }

    /// File ▸ Open Project…: asks the front end for a folder, then opens it.
    pub fn choose_and_open_project(&mut self) {
        let id = DialogId(self.next());
        self.dialogs.insert(id, PendingDialog::OpenProject);
        self.front.dialogs.choose_project_folder(id);
    }

    /// Closes the project's window after saving its editor. Its lock is released here. If the
    /// save fails the window stays open (an alert says why), so no edit is lost.
    pub fn close_project(&mut self, key: ProjectKey) -> bool {
        if !self.flush_or_alert(key) {
            return false;
        }
        let Some(i) = self.projects.iter().position(|(k, _)| *k == key) else {
            return true;
        };
        self.projects.remove(i);
        self.stop_timers(key);
        self.events.push(Event::ProjectClosed { project: key });
        self.update_welcome();
        true
    }

    /// Saves every editor and writes which projects are open, for the next launch. Projects
    /// stay open: the front end terminates after this returns. `false` means an editor could
    /// not be saved (already alerted); the front end should cancel termination.
    pub fn quit(&mut self) -> Result<bool, ModelError> {
        let saved = self.save_all();
        self.save_state()?;
        Ok(saved)
    }

    pub fn project(&self, key: ProjectKey) -> Option<&ProjectWindow> {
        self.projects
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, w)| w)
    }

    /// Open projects in window order.
    pub fn projects(&self) -> impl Iterator<Item = (ProjectKey, &ProjectWindow)> {
        self.projects.iter().map(|(k, w)| (*k, w))
    }

    /// Most recent first.
    pub fn recent_projects(&self) -> &[PathBuf] {
        &self.recent
    }

    /// File ▸ Open Recent ▸ Clear Menu. Written to `state.json` with the open projects, at
    /// quit.
    pub fn clear_recent_projects(&mut self) {
        if !self.recent.is_empty() {
            self.recent.clear();
            self.events.push(Event::RecentProjectsChanged);
        }
    }

    /// The welcome window's Remove from List: forgets one recent project (by its index in
    /// [`recent_projects`](Self::recent_projects)); its folder is left alone. Written to
    /// `state.json` at quit, like Clear Menu.
    pub fn remove_recent_project(&mut self, index: usize) {
        if index < self.recent.len() {
            self.recent.remove(index);
            self.events.push(Event::RecentProjectsChanged);
        }
    }

    pub fn welcome_visible(&self) -> bool {
        self.projects.is_empty()
    }

    /// Everything that changed since the last call, in order.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Runs `work` on a new thread and `then` with its result on the main thread, during the
    /// [`pump`](Self::pump) that follows the worker's wake.
    pub fn spawn<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&mut App, T) + Send + 'static,
    ) {
        self.jobs += 1;
        let done = self.done_tx.clone();
        let main = self.front.main_thread.clone();
        std::thread::spawn(move || {
            let result = work();
            let completion: Completion = Box::new(move |app| then(app, result));
            // The app is gone if this fails; nothing is left to tell.
            if done.send(completion).is_ok() {
                main.wake();
            }
        });
    }

    /// Applies finished background work. The front end calls this on the main thread after
    /// [`MainThread::wake`](crate::MainThread::wake).
    pub fn pump(&mut self) {
        while let Ok(completion) = self.done_rx.try_recv() {
            self.jobs -= 1;
            completion(self);
        }
    }

    /// Workers whose results have not been applied yet, e.g. for a progress indicator.
    pub fn jobs_running(&self) -> usize {
        self.jobs
    }

    /// The front end reports the answer to a dialog. Unknown ids are ignored, so a dialog that
    /// outlived its purpose does no harm.
    pub fn dialog_answered(&mut self, id: DialogId, answer: DialogAnswer) {
        let Some(pending) = self.dialogs.remove(&id) else {
            return;
        };
        match (pending, answer) {
            (PendingDialog::OpenProject, DialogAnswer::Folder(folder)) => {
                if let Err(e) = self.open_project(&folder) {
                    self.alert_error(&format!("Could not open {}", folder.display()), &e);
                }
            }
            (PendingDialog::DeleteRequest { project, request }, DialogAnswer::Confirmed) => {
                match self.delete_request_now(project, request) {
                    // The window was closed while the sheet was up.
                    Ok(()) | Err(ModelError::UnknownProject) => {}
                    Err(e) => self.alert_error("Could not delete the request", &e),
                }
            }
            (PendingDialog::DeleteServer { project, server }, DialogAnswer::Confirmed) => {
                match self.delete_server(project, server) {
                    Ok(()) | Err(ModelError::UnknownProject) => {}
                    Err(e) => self.alert_error("Could not delete the server", &e),
                }
            }
            (_, DialogAnswer::Cancelled) => {}
            // An answer that doesn't fit the question; the front end has a bug, ignore it.
            (_, DialogAnswer::Folder(_) | DialogAnswer::Confirmed) => {}
        }
    }

    pub(crate) fn add_project(&mut self, project: Project, restore: OpenProject) -> ProjectKey {
        let key = ProjectKey(self.next());
        // The folder name is a fine label if the database can't say.
        let name = project.name().unwrap_or_else(|_| {
            restore
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let entry = project.entry_wsdl();
        let wsdl_dir = project.wsdl_dir();
        let mut window = ProjectWindow {
            project,
            name,
            restore,
            schema: SchemaState::Loading,
            sidebar: Sidebar::default(),
            servers: Vec::new(),
            editor: None,
            history: Vec::new(),
            response: None,
            sending: None,
            suggested_servers: Vec::new(),
            replace_outcome: None,
        };
        // A database that fails here fails again on the first command, which reports it.
        let _ = window.reload_requests();
        let _ = window.reload_servers();
        if window.selected_request().is_none() {
            window.restore.last_selected_request = window.sidebar.requests.first().map(|r| r.id);
        }
        self.projects.push((key, window));
        self.events.push(Event::ProjectOpened { project: key });
        self.load_editor(key);

        match entry {
            Ok(entry) => self.spawn(
                move || ProjectSchema::load(&entry, wsdl_dir),
                move |app, loaded| app.schema_loaded(key, loaded),
            ),
            Err(e) => self.schema_loaded(key, Err(e.to_string())),
        }
        key
    }

    pub(crate) fn schema_loaded(&mut self, key: ProjectKey, loaded: Result<ProjectSchema, String>) {
        // The project may have been closed while its WSDL loaded.
        let Some(window) = self.window_mut(key) else {
            return;
        };
        match loaded {
            Ok(schema) => {
                window.sidebar.services = operation_tree(&schema.wsdl);
                window.schema = SchemaState::Ready(std::sync::Arc::new(schema));
            }
            Err(message) => window.schema = SchemaState::Failed(message),
        }
        self.events.push(Event::SidebarChanged { project: key });
        self.start_check(key, Check::Full);
    }

    /// For commands: a project closed in the meantime is [`ModelError::UnknownProject`].
    pub(crate) fn window(&mut self, key: ProjectKey) -> Result<&mut ProjectWindow, ModelError> {
        self.window_mut(key).ok_or(ModelError::UnknownProject)
    }

    pub(crate) fn window_mut(&mut self, key: ProjectKey) -> Option<&mut ProjectWindow> {
        self.projects
            .iter_mut()
            .find(|(k, _)| *k == key)
            .map(|(_, w)| w)
    }

    fn key_for_path(&self, folder: &Path) -> Option<ProjectKey> {
        self.projects
            .iter()
            .find(|(_, w)| same_folder(w.path(), folder))
            .map(|(k, _)| *k)
    }

    pub(crate) fn note_recent(&mut self, folder: &Path) {
        let mut state = AppState {
            open_projects: Vec::new(),
            recent_projects: std::mem::take(&mut self.recent),
        };
        state.note_recent(folder);
        self.recent = state.recent_projects;
        self.events.push(Event::RecentProjectsChanged);
    }

    fn save_state(&self) -> Result<(), ModelError> {
        let state = AppState {
            open_projects: self
                .projects
                .iter()
                .map(|(_, w)| w.restore.clone())
                .collect(),
            recent_projects: self.recent.clone(),
        };
        state.save(&self.state_dir)?;
        Ok(())
    }

    pub(crate) fn update_welcome(&mut self) {
        let visible = self.welcome_visible();
        if self.welcome_visible != Some(visible) {
            self.welcome_visible = Some(visible);
            self.events.push(Event::WelcomeVisibility { visible });
        }
    }

    /// Shows `error` as an alert through the front end's [`Dialogs`](crate::Dialogs). For
    /// errors a command returned to the front end, so they look like the model's own.
    pub fn alert_error(&self, title: &str, error: &ModelError) {
        self.front.dialogs.alert(Alert {
            title: title.into(),
            message: error.to_string(),
        });
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// Paths name the same folder. Falls back to comparing as given when either can't be resolved
/// (a folder that vanished is still "the same" as its old path).
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
