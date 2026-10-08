//! Per-project window state: the sidebar tree, selection, and the server popup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use washboard_core::model::{OperationRef, RequestId, Server, ServerId};
use washboard_core::project::{OpenProject, Project};
use washboard_core::schema::SchemaModel;
use washboard_core::wsdl::{self, Sources, Support, Wsdl};

use crate::editor::Editor;
use crate::front_end::TimerId;

/// The project's WSDL and the schema model built from it, loaded off the main thread and then
/// shared read-only.
#[derive(Debug)]
pub struct ProjectSchema {
    pub wsdl: Wsdl,
    pub model: SchemaModel,
}

impl ProjectSchema {
    /// Reads and analyzes the WSDL set; runs on a worker.
    pub(crate) fn load(entry: &Path, wsdl_dir: PathBuf) -> Result<ProjectSchema, String> {
        let sources = Sources::from_disk(entry, &[wsdl_dir]).map_err(|e| e.to_string())?;
        let wsdl = wsdl::load(&sources);
        let model = SchemaModel::build(&wsdl.bundle);
        Ok(ProjectSchema { wsdl, model })
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
    /// The running autosave timer, if an edit is waiting to be saved.
    pub(crate) autosave: Option<TimerId>,
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

    /// The server popup's items, in the project's order.
    pub fn servers(&self) -> &[Server] {
        &self.servers
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
                .map(|port| PortNode {
                    name: port.name.clone(),
                    operations: wsdl
                        .definitions
                        .binding(&port.binding)
                        .map(|b| {
                            b.operations
                                .iter()
                                .map(|op| OperationNode {
                                    operation: OperationRef {
                                        binding: b.name.clone(),
                                        operation: op.name.clone(),
                                    },
                                    unsupported: match &op.support {
                                        Support::Supported => None,
                                        Support::Unsupported(why) => Some(why.to_string()),
                                    },
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
        })
        .collect()
}
