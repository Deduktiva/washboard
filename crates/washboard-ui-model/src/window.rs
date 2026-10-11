//! Per-project window state: the sidebar tree, selection, and the server popup.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::Chars;
use std::sync::{Arc, Mutex, MutexGuard};

use washboard_core::diag::Diagnostic;
use washboard_core::http;
use washboard_core::model::{HistoryEntry, OperationRef, RequestId, Server, ServerId};
use washboard_core::project::{OpenProject, Project};
use washboard_core::schema::SchemaModel;
use washboard_core::validate::request::RequestSchema;
use washboard_core::wsdl::{self, Direction, Protocol, Sources, Style, Wsdl};

use crate::app::ModelError;
use crate::diagnostics::WellFormedness;
use crate::editor::Editor;
use crate::import::ReplaceOutcome;
use crate::send::{OlderExchange, ResponseView, Sending};
use crate::watch::RequestWatcher;

/// The project's WSDL, the schema model built from it, and the compiled request schema, loaded
/// off the main thread and then shared read-only with the main thread and workers.
#[derive(Debug)]
pub struct ProjectSchema {
    pub wsdl: Wsdl,
    pub model: Arc<SchemaModel>,
    /// libxml2 schemas are `Send` but not `Sync`; validations of one project take turns.
    request: Result<Mutex<RequestSchema>, Vec<Diagnostic>>,
}

impl ProjectSchema {
    /// Reads and analyzes the WSDL set and compiles its schemas; runs on a worker.
    pub(crate) fn load(entry: &Path, wsdl_dir: PathBuf) -> Result<ProjectSchema, String> {
        let sources = Sources::from_disk(entry, &[wsdl_dir]).map_err(|e| e.to_string())?;
        let wsdl = wsdl::load(&sources);
        let model = Arc::new(SchemaModel::build(&wsdl.bundle));
        let request = RequestSchema::compile(&wsdl.bundle)
            .map(|schema| Mutex::new(schema.with_model(model.clone())));
        Ok(ProjectSchema {
            wsdl,
            model,
            request,
        })
    }

    /// Why requests can't be schema-validated, if they can't.
    pub fn compile_errors(&self) -> &[Diagnostic] {
        match &self.request {
            Ok(_) => &[],
            Err(errors) => errors,
        }
    }

    /// `None` if the schemas did not compile. A validation that panicked doesn't poison it for
    /// good: libxml2 keeps no state between validations.
    pub(crate) fn request_schema(&self) -> Option<MutexGuard<'_, RequestSchema>> {
        let mutex = self.request.as_ref().ok()?;
        Some(
            mutex
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

/// Where the project's schema stands.
#[derive(Debug, Clone)]
pub enum SchemaState {
    Loading,
    Ready(Arc<ProjectSchema>),
    /// The WSDL set could not be read; the message is for the sidebar's placeholder.
    Failed(String),
}

impl SchemaState {
    /// The loaded WSDL, for commands that need it; the error says why there is none.
    pub(crate) fn ready(&self) -> Result<&Arc<ProjectSchema>, ModelError> {
        match self {
            SchemaState::Ready(schema) => Ok(schema),
            SchemaState::Loading => Err(ModelError::SchemaNotReady),
            SchemaState::Failed(m) => Err(ModelError::SchemaFailed(m.clone())),
        }
    }

    /// The loaded WSDL with schemas that compiled, so requests can be validated against it.
    pub(crate) fn validating(&self) -> Result<&Arc<ProjectSchema>, ModelError> {
        let schema = self.ready()?;
        if schema.compile_errors().is_empty() {
            Ok(schema)
        } else {
            Err(ModelError::SchemaFailed(
                "the WSDL's schemas could not be compiled".into(),
            ))
        }
    }
}

/// What the sidebar shows (PLAN §8): the requests in their folders, and the operations the
/// operation picker offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sidebar {
    /// Every request, by name in Finder's order: case-insensitive, numbers by value. The
    /// database's `sort_order` is not used.
    pub requests: Vec<RequestRow>,
    /// The REQUESTS tree: the top level, its folders and requests.
    pub tree: FolderNode,
    /// Service › port › operation. Empty while the schema loads.
    pub services: Vec<ServiceNode>,
}

impl Sidebar {
    pub fn request(&self, id: RequestId) -> Option<&RequestRow> {
        self.requests.iter().find(|r| r.id == id)
    }

    /// The requests in the tree's order: each folder's folders, then its requests.
    pub fn request_order(&self) -> Vec<RequestId> {
        let mut order = Vec::with_capacity(self.requests.len());
        self.tree.collect_requests(&mut order);
        order
    }

    pub(crate) fn request_mut(&mut self, id: RequestId) -> Option<&mut RequestRow> {
        self.requests.iter_mut().find(|r| r.id == id)
    }
}

/// A folder of the REQUESTS tree. The tree's root is the top level of `requests/`, with an
/// empty name and path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderNode {
    pub name: String,
    /// Relative to `requests/`, `/`-separated.
    pub path: String,
    /// By name in Finder's order.
    pub folders: Vec<FolderNode>,
    /// By name in Finder's order; rows in [`Sidebar::requests`].
    pub requests: Vec<RequestId>,
}

impl FolderNode {
    /// The folder at `path` in this tree (`""` is the root).
    pub fn folder(&self, path: &str) -> Option<&FolderNode> {
        if path == self.path {
            return Some(self);
        }
        self.folders
            .iter()
            .find(|f| path == f.path || path.starts_with(&format!("{}/", f.path)))
            .and_then(|f| f.folder(path))
    }

    /// Requests in this folder and all folders inside it.
    pub fn request_count(&self) -> usize {
        self.requests.len()
            + self
                .folders
                .iter()
                .map(FolderNode::request_count)
                .sum::<usize>()
    }

    fn collect_requests(&self, into: &mut Vec<RequestId>) {
        for f in &self.folders {
            f.collect_requests(into);
        }
        into.extend(&self.requests);
    }

    /// The tree of `folders` (every folder path, as [`Project::folders`] lists them) with the
    /// `requests` in their folders.
    fn build(folders: &[String], requests: &[RequestRow]) -> FolderNode {
        let mut root = FolderNode::default();
        for path in folders {
            root.insert_folder(path);
        }
        for r in requests {
            // A request's folder exists: both come from the same walk of the disk. Should a
            // file appear in between, the folder is added.
            root.insert_folder(&r.folder);
            if let Some(f) = root.folder_mut(&r.folder) {
                f.requests.push(r.id);
            }
        }
        root.sort(requests);
        root
    }

    fn insert_folder(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        let mut node = self;
        let mut so_far = String::new();
        for component in path.split('/') {
            so_far = if so_far.is_empty() {
                component.to_owned()
            } else {
                format!("{so_far}/{component}")
            };
            let i = match node.folders.iter().position(|f| f.path == so_far) {
                Some(i) => i,
                None => {
                    node.folders.push(FolderNode {
                        name: component.to_owned(),
                        path: so_far.clone(),
                        ..FolderNode::default()
                    });
                    node.folders.len() - 1
                }
            };
            node = &mut node.folders[i];
        }
    }

    fn folder_mut(&mut self, path: &str) -> Option<&mut FolderNode> {
        if path == self.path {
            return Some(self);
        }
        self.folders
            .iter_mut()
            .find(|f| path == f.path || path.starts_with(&format!("{}/", f.path)))
            .and_then(|f| f.folder_mut(path))
    }

    fn sort(&mut self, rows: &[RequestRow]) {
        self.folders.sort_by(|a, b| finder_order(&a.name, &b.name));
        let name = |id: &RequestId| {
            rows.iter()
                .find(|r| r.id == *id)
                .map_or("", |r| r.name.as_str())
        };
        self.requests.sort_by(|a, b| finder_order(name(a), name(b)));
        for f in &mut self.folders {
            f.sort(rows);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRow {
    pub id: RequestId,
    pub name: String,
    /// Its folder's path; `""` at the top level.
    pub folder: String,
    /// Edited and not yet saved (the • marker).
    pub dirty: bool,
    /// Fails validation (the ⚠ marker).
    pub invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceNode {
    pub name: String,
    pub ports: Vec<PortNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortNode {
    pub name: String,
    /// The binding's protocol, for the port's "1.1" or "1.2 · unsupported" chip; `None` when
    /// the port names a binding the WSDL lacks.
    pub protocol: Option<Protocol>,
    pub operations: Vec<OperationNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationNode {
    pub operation: OperationRef,
    /// Why the operation can't be used (SOAP 1.2, rpc/encoded, …); shown, never hidden.
    pub unsupported: Option<String>,
    /// The input element, as the operation picker shows it: `prefix:local` with the prefix the
    /// schemas use, or "rpc" for rpc style, whose wrapper is named after the operation. Empty
    /// for an operation without an input element.
    pub input: String,
}

impl OperationNode {
    pub fn name(&self) -> &str {
        &self.operation.operation
    }
}

/// A SOAP 1.1 port's `soap:address`, listed with the servers but never one itself: it is only
/// sent to once the user has added it as a server (PLAN §4 "Create project", step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsdlServer {
    pub port: String,
    pub url: String,
}

/// What the request bar shows for the open request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSummary {
    pub name: String,
    /// The operation the request was made for; `None` for a request without the hint.
    pub operation: Option<OperationRef>,
    pub well_formedness: WellFormedness,
}

/// One open project and the state of its window.
#[derive(Debug)]
pub struct ProjectWindow {
    pub(crate) project: Project,
    pub(crate) name: String,
    pub(crate) restore: OpenProject,
    pub(crate) schema: SchemaState,
    pub(crate) sidebar: Sidebar,
    pub(crate) servers: Vec<Server>,
    pub(crate) editor: Option<Editor>,
    pub(crate) history: Vec<HistoryEntry>,
    pub(crate) response: Option<ResponseView>,
    pub(crate) older: Option<OlderExchange>,
    pub(crate) sending: Option<Sending>,
    pub(crate) replace_outcome: Option<ReplaceOutcome>,
    /// `None` while watching is off or could not start.
    pub(crate) watcher: Option<RequestWatcher>,
    /// Folder paths the user collapsed; kept in `ui_state`.
    pub(crate) collapsed: BTreeSet<String>,
}

impl ProjectWindow {
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The project's display name, read once when it was opened.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.restore.path
    }

    pub fn schema(&self) -> &SchemaState {
        &self.schema
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.sidebar
    }

    /// The folders the user collapsed in the sidebar, by path. Kept per project.
    pub fn collapsed_folders(&self) -> &BTreeSet<String> {
        &self.collapsed
    }

    /// Whether [`App::move_request`](crate::App::move_request) would move `request` into
    /// `folder`, for drag and drop: a drop the model refuses is not offered.
    pub fn can_move_request(&self, request: RequestId, folder: &str) -> bool {
        self.project.check_move_request(request, folder).is_ok()
    }

    /// Whether [`App::move_folder`](crate::App::move_folder) would move the folder at `path`
    /// into `parent`.
    pub fn can_move_folder(&self, path: &str, parent: &str) -> bool {
        self.project.check_move_folder(path, parent).is_ok()
    }

    pub fn selected_request(&self) -> Option<RequestId> {
        self.restore.last_selected_request
    }

    /// The selected request's buffer; `None` without a selection or if it could not be read.
    pub fn editor(&self) -> Option<&Editor> {
        self.editor.as_ref()
    }

    /// The open request's name, operation and well-formedness; `None` with no request open.
    pub fn request_summary(&self) -> Option<RequestSummary> {
        let editor = self.editor.as_ref()?;
        let id = editor.request();
        Some(RequestSummary {
            name: self.sidebar.request(id)?.name.clone(),
            operation: self.request_operation(id),
            well_formedness: editor.well_formedness(),
        })
    }

    /// The operation `request` was made for. A hint only, kept in the project database: it
    /// may name an operation the WSDL no longer has.
    pub(crate) fn request_operation(&self, request: RequestId) -> Option<OperationRef> {
        self.project.request(request).ok().and_then(|r| r.operation)
    }

    /// Has unsaved edits: the window's edited dot.
    pub fn edited(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.dirty())
    }

    /// The loaded WSDL's SOAP 1.1 addresses, in document order; empty while the schema loads
    /// or when it failed. Read from the WSDL each time, never stored: the WSDL is the record.
    pub fn wsdl_servers(&self) -> Vec<WsdlServer> {
        let SchemaState::Ready(schema) = &self.schema else {
            return Vec::new();
        };
        schema
            .wsdl
            .soap11_addresses()
            .into_iter()
            .map(|(port, url)| WsdlServer { port, url })
            .collect()
    }

    /// What the last Replace WSDL changed.
    pub fn replace_outcome(&self) -> Option<&ReplaceOutcome> {
        self.replace_outcome.as_ref()
    }

    /// The server popup's items, in the project's order.
    pub fn servers(&self) -> &[Server] {
        &self.servers
    }

    /// How a response or history entry names its server: the server's current name, or the
    /// host of the URL it went to when that server has been deleted since
    /// ([`http::host_label`]; the URL itself if it doesn't parse).
    pub fn server_label(&self, server: Option<ServerId>, url: &str) -> String {
        server
            .and_then(|id| self.servers.iter().find(|s| s.id == id))
            .map_or_else(
                || http::host_label(url).unwrap_or_else(|| url.to_owned()),
                |s| s.name.clone(),
            )
    }

    /// The server popup's selection: the selected request's last server, else the project's
    /// most recently used one, else the first.
    pub fn selected_server(&self) -> Option<ServerId> {
        let from_request = self
            .selected_request()
            .and_then(|id| self.project.request(id).ok().and_then(|r| r.last_server));
        let from_project = || self.project.last_used_server().ok().flatten();
        from_request
            .or_else(from_project)
            .filter(|id| self.servers.iter().any(|s| s.id == *id))
            .or_else(|| self.servers.first().map(|s| s.id))
    }

    /// Re-reads the request rows, sorted by name, and the folder tree, keeping dirty and
    /// invalid markers of requests that still exist.
    pub(crate) fn reload_requests(&mut self) -> Result<(), washboard_core::project::ProjectError> {
        let folders = self.project.folders()?;
        let old = std::mem::take(&mut self.sidebar.requests);
        let mut rows: Vec<RequestRow> = self
            .project
            .requests()?
            .into_iter()
            .map(|r| {
                let prev = old.iter().find(|o| o.id == r.id);
                RequestRow {
                    id: r.id,
                    name: r.name,
                    folder: r.folder,
                    dirty: prev.is_some_and(|p| p.dirty),
                    invalid: prev.is_some_and(|p| p.invalid),
                }
            })
            .collect();
        rows.sort_by(|a, b| finder_order(&a.name, &b.name));
        self.sidebar.tree = FolderNode::build(&folders, &rows);
        self.sidebar.requests = rows;
        if let Some(sel) = self.selected_request()
            && self.sidebar.request(sel).is_none()
        {
            self.restore.last_selected_request = None;
        }
        Ok(())
    }

    /// Reads the collapsed folders from `ui_state`; a database that fails reads as none.
    pub(crate) fn load_collapsed(&mut self) {
        let stored = self.project.ui_state(COLLAPSED_KEY).ok().flatten();
        self.collapsed = stored
            .iter()
            .flat_map(|s| s.split('\n'))
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
    }

    /// Writes the collapsed folders to `ui_state`, one path per line: a folder name has no
    /// control characters.
    pub(crate) fn save_collapsed(&mut self) -> Result<(), washboard_core::project::ProjectError> {
        let value = self
            .collapsed
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        self.project
            .set_ui_state(COLLAPSED_KEY, (!value.is_empty()).then_some(value.as_str()))
    }

    /// Shows the folder at `path` expanded, and every folder it is in, so a row created or
    /// moved there is in view.
    pub(crate) fn expand(
        &mut self,
        path: &str,
    ) -> Result<(), washboard_core::project::ProjectError> {
        let before = self.collapsed.len();
        self.collapsed
            .retain(|p| !(path == p || path.starts_with(&format!("{p}/"))));
        if self.collapsed.len() == before {
            return Ok(());
        }
        self.save_collapsed()
    }

    /// After the folder at `from` became `to`: collapsed folders at or inside it follow.
    pub(crate) fn folder_moved(&mut self, from: &str, to: &str) {
        let moved: Vec<String> = self
            .collapsed
            .iter()
            .filter(|p| *p == from || p.starts_with(&format!("{from}/")))
            .cloned()
            .collect();
        for path in moved {
            self.collapsed.remove(&path);
            self.collapsed
                .insert(format!("{to}{}", &path[from.len()..]));
        }
    }

    pub(crate) fn mark_dirty(&mut self, request: RequestId, dirty: bool) {
        if let Some(row) = self.sidebar.request_mut(request) {
            row.dirty = dirty;
        }
    }

    pub(crate) fn reload_servers(&mut self) -> Result<(), washboard_core::project::ProjectError> {
        self.servers = self.project.servers()?;
        Ok(())
    }
}

/// `ui_state` key of the collapsed folders.
const COLLAPSED_KEY: &str = "sidebar.collapsed_folders";

/// Finder's order for names: case-insensitive, and runs of digits compared by value, so
/// "Lookup 2" comes before "lookup 10". Equal names under those rules fall back to plain
/// string order, so the order never depends on the database's.
pub(crate) fn finder_order(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    while let (Some(&cx), Some(&cy)) = (x.peek(), y.peek()) {
        let ord = if cx.is_ascii_digit() && cy.is_ascii_digit() {
            let (nx, ny) = (digits(&mut x), digits(&mut y));
            let (tx, ty) = (nx.trim_start_matches('0'), ny.trim_start_matches('0'));
            tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
        } else {
            x.next();
            y.next();
            cx.to_lowercase().cmp(cy.to_lowercase())
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    x.peek()
        .is_some()
        .cmp(&y.peek().is_some())
        .then_with(|| a.cmp(b))
}

/// The run of ASCII digits at the front of `chars`, taken from it.
fn digits(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        run.push(c);
    }
    run
}

/// The operations section from the WSDL's services, in document order.
pub(crate) fn operation_tree(wsdl: &Wsdl, model: &SchemaModel) -> Vec<ServiceNode> {
    wsdl.definitions
        .services
        .iter()
        .map(|service| ServiceNode {
            name: service.name.local.clone(),
            ports: service
                .ports
                .iter()
                .map(|port| {
                    let binding = wsdl.definitions.binding(&port.binding);
                    PortNode {
                        name: port.name.clone(),
                        protocol: binding.map(|b| b.protocol.clone()),
                        operations: binding
                            .map(|b| {
                                b.operations
                                    .iter()
                                    .map(|op| OperationNode {
                                        operation: b.operation_ref(op),
                                        unsupported: op.support.reason().map(ToString::to_string),
                                        input: input_label(op, model),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    }
                })
                .collect(),
        })
        .collect()
}

fn input_label(op: &wsdl::Operation, model: &SchemaModel) -> String {
    if op.style == Style::Rpc {
        return "rpc".into();
    }
    let Some(element) = op.body_elements(Direction::Input).into_iter().next() else {
        return String::new();
    };
    match model
        .prefix_hints()
        .iter()
        .find(|(ns, _)| *ns == element.ns)
    {
        Some((_, prefix)) => format!("{prefix}:{}", element.local),
        None => element.local,
    }
}
