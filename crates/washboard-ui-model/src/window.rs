//! Per-project window state: the sidebar tree, selection, and the server popup.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use washboard_core::diag::Diagnostic;
use washboard_core::http;
use washboard_core::model::{HistoryEntry, OperationRef, RequestId, Server, ServerId};
use washboard_core::project::{OpenProject, Project};
use washboard_core::schema::SchemaModel;
use washboard_core::validate::request::RequestSchema;
use washboard_core::wsdl::{self, Protocol, Sources, Wsdl};

use crate::app::ModelError;
use crate::editor::Editor;
use crate::import::{ReplaceOutcome, SuggestedServer};
use crate::send::{ResponseView, Sending};

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
    pub requests: Vec<RequestRow>,
    /// Service › port › operation. Empty while the schema loads.
    pub services: Vec<ServiceNode>,
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

    /// Re-reads the request rows, keeping dirty and invalid markers of requests that still
    /// exist.
    pub(crate) fn reload_requests(&mut self) -> Result<(), washboard_core::project::ProjectError> {
        let old = std::mem::take(&mut self.sidebar.requests);
        self.sidebar.requests = self
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
        if let Some(sel) = self.selected_request()
            && !self.sidebar.requests.iter().any(|r| r.id == sel)
        {
            self.restore.last_selected_request = None;
        }
        Ok(())
    }

    pub(crate) fn mark_dirty(&mut self, request: RequestId, dirty: bool) {
        if let Some(row) = self.sidebar.requests.iter_mut().find(|r| r.id == request) {
            row.dirty = dirty;
        }
    }

    pub(crate) fn reload_servers(&mut self) -> Result<(), washboard_core::project::ProjectError> {
        self.servers = self.project.servers()?;
        Ok(())
    }
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
                                        operation: OperationRef {
                                            binding: b.name.clone(),
                                            operation: op.name.clone(),
                                        },
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
