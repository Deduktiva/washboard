//! Per-project window state: the sidebar tree, selection, and the server popup.

use std::cmp::Ordering;
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
use washboard_core::wsdl::{self, Protocol, Sources, Wsdl};

use crate::app::ModelError;
use crate::diagnostics::WellFormedness;
use crate::editor::Editor;
use crate::import::{ReplaceOutcome, SuggestedServer};
use crate::send::{OlderExchange, ResponseView, Sending};

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

/// The sidebar's two sections (PLAN §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sidebar {
    /// By name in Finder's order: case-insensitive, numbers by value. The database's
    /// `sort_order` is not used.
    pub requests: Vec<RequestRow>,
    /// Service › port › operation. Empty while the schema loads.
    pub services: Vec<ServiceNode>,
}

impl Sidebar {
    pub fn request(&self, id: RequestId) -> Option<&RequestRow> {
        self.requests.iter().find(|r| r.id == id)
    }

    pub(crate) fn request_mut(&mut self, id: RequestId) -> Option<&mut RequestRow> {
        self.requests.iter_mut().find(|r| r.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRow {
    pub id: RequestId,
    pub name: String,
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
}

impl OperationNode {
    pub fn name(&self) -> &str {
        &self.operation.operation
    }
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
    pub(crate) suggested_servers: Vec<SuggestedServer>,
    pub(crate) replace_outcome: Option<ReplaceOutcome>,
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

    /// Servers from the WSDL's addresses, not added until the user confirms them.
    pub fn suggested_servers(&self) -> &[SuggestedServer] {
        &self.suggested_servers
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

    /// Re-reads the request rows, sorted by name, keeping dirty and invalid markers of
    /// requests that still exist.
    pub(crate) fn reload_requests(&mut self) -> Result<(), washboard_core::project::ProjectError> {
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
                    dirty: prev.is_some_and(|p| p.dirty),
                    invalid: prev.is_some_and(|p| p.invalid),
                }
            })
            .collect();
        rows.sort_by(|a, b| finder_order(&a.name, &b.name));
        self.sidebar.requests = rows;
        if let Some(sel) = self.selected_request()
            && self.sidebar.request(sel).is_none()
        {
            self.restore.last_selected_request = None;
        }
        Ok(())
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
pub(crate) fn operation_tree(wsdl: &Wsdl) -> Vec<ServiceNode> {
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
