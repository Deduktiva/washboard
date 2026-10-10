//! Project window commands (PLAN §4 "Requests", §8 "Project settings → Servers"). Every
//! change goes straight to the project folder; the window's state is re-read from it so the
//! sidebar always shows what is on disk.

use std::time::Duration;

use washboard_core::model::{Auth, OperationRef, RequestId, Server, ServerId};
use washboard_core::schema::TemplateOptions;
use washboard_core::soap;

use crate::app::{App, ModelError, PendingDialog, ProjectKey};
use crate::event::Event;
use crate::front_end::{Confirm, DialogId};
use crate::timers::TimerKind;

/// A new server's timeout until the user changes it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

impl App {
    /// Selects `request` (or nothing) and opens it in the editor. Remembered for the next
    /// launch. The previous request is saved first; if that fails, the selection stays and the
    /// error says why.
    pub fn select_request(
        &mut self,
        key: ProjectKey,
        request: Option<RequestId>,
    ) -> Result<(), ModelError> {
        if self.window(key)?.restore.last_selected_request == request {
            return Ok(());
        }
        self.flush(key)?;
        self.window(key)?.restore.last_selected_request = request;
        self.events.push(Event::SelectionChanged { project: key });
        self.events
            .push(Event::ServerSelectionChanged { project: key });
        self.load_editor(key);
        self.events.push(Event::EditorReplaced { project: key });
        Ok(())
    }

    /// Project ▸ New Request, or a double-click on an operation: generates the body from the
    /// schema, names it `<Operation> <n>`, selects it and starts inline rename.
    pub fn new_request(
        &mut self,
        key: ProjectKey,
        operation: &OperationRef,
    ) -> Result<RequestId, ModelError> {
        let opts = TemplateOptions {
            indent: " ".repeat(self.format.indent),
            ..TemplateOptions::default()
        };
        let window = self.window(key)?;
        let schema = window.schema.ready()?.clone();
        let text = soap::request_envelope(&schema.wsdl, &schema.model, operation, &opts)?;
        let meta = window.project.create_request(operation, &text)?;
        self.requests_changed(key)?;
        self.select_request(key, Some(meta.id))?;
        self.events.push(Event::BeginRename {
            project: key,
            request: meta.id,
        });
        Ok(meta.id)
    }

    /// What Project ▸ New Request creates a request for when no operation is selected in the
    /// sidebar: the first supported operation, in the sidebar's order.
    pub fn default_operation(&self, key: ProjectKey) -> Result<OperationRef, ModelError> {
        let window = self.project(key).ok_or(ModelError::UnknownProject)?;
        window.schema.ready()?;
        window
            .sidebar
            .services
            .iter()
            .flat_map(|s| &s.ports)
            .flat_map(|p| &p.operations)
            .find(|o| o.unsupported.is_none())
            .map(|o| o.operation.clone())
            .ok_or(ModelError::NoSupportedOperation)
    }

    /// Inline rename. Name errors (empty, taken, `/` …) come back for the front end to show
    /// next to the field; the old name stays.
    pub fn rename_request(
        &mut self,
        key: ProjectKey,
        request: RequestId,
        name: &str,
    ) -> Result<(), ModelError> {
        self.window(key)?.project.rename_request(request, name)?;
        self.requests_changed(key)
    }

    /// Project ▸ Duplicate: `<name> copy`, then selected. Unsaved edits are saved first so the
    /// copy has them.
    pub fn duplicate_request(
        &mut self,
        key: ProjectKey,
        request: RequestId,
    ) -> Result<RequestId, ModelError> {
        self.flush(key)?;
        let meta = self.window(key)?.project.duplicate_request(request)?;
        self.requests_changed(key)?;
        self.select_request(key, Some(meta.id))?;
        Ok(meta.id)
    }

    /// Project ▸ Delete: asks first, since the request's history goes with it.
    pub fn delete_request(
        &mut self,
        key: ProjectKey,
        request: RequestId,
    ) -> Result<(), ModelError> {
        let name = self.window(key)?.project.request(request)?.name;
        let id = DialogId(self.next());
        self.dialogs.insert(
            id,
            PendingDialog::DeleteRequest {
                project: key,
                request,
            },
        );
        self.front.dialogs.confirm(
            id,
            Confirm {
                title: format!("Delete “{name}”?"),
                message: "The request and its response history are deleted. This can't be \
                          undone."
                    .into(),
                action: "Delete".into(),
            },
        );
        Ok(())
    }

    /// After the confirmation. The next request (else the previous) is selected.
    pub(crate) fn delete_request_now(
        &mut self,
        key: ProjectKey,
        request: RequestId,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let rows = &window.sidebar.requests;
        let neighbour = rows.iter().position(|r| r.id == request).and_then(|i| {
            rows.get(i + 1)
                .or_else(|| i.checked_sub(1).and_then(|p| rows.get(p)))
                .map(|r| r.id)
        });
        let was_selected = window.selected_request() == Some(request);
        window.project.delete_request(request)?;
        if window
            .editor
            .as_ref()
            .is_some_and(|e| e.request() == request)
        {
            // Its edits go with it.
            window.editor = None;
            self.stop_timer(key, TimerKind::Autosave);
        }
        self.requests_changed(key)?;
        if was_selected {
            self.select_request(key, neighbour)?;
        }
        Ok(())
    }

    /// The toolbar's server popup. Remembered on the selected request, which is also what a
    /// new request starts with (PLAN §4 "Requests").
    pub fn choose_server(&mut self, key: ProjectKey, server: ServerId) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let Some(request) = window.selected_request() else {
            return Ok(());
        };
        window
            .project
            .set_request_last_server(request, Some(server))?;
        self.events
            .push(Event::ServerSelectionChanged { project: key });
        Ok(())
    }

    /// Settings ▸ Servers ▸ +: a server the user then fills in.
    pub fn add_server(&mut self, key: ProjectKey) -> Result<ServerId, ModelError> {
        let window = self.window(key)?;
        let server = Server {
            id: ServerId::new(),
            name: "New Server".into(),
            url: "https://".into(),
            ignore_tls_errors: false,
            auth: Auth::None,
            timeout: DEFAULT_TIMEOUT,
        };
        window.project.add_server(&server)?;
        self.servers_changed(key)?;
        Ok(server.id)
    }

    /// Saves the settings form for one server. `password` is only looked at with Basic auth:
    /// `Some` stores it, `None` leaves the stored one alone. Switching to no auth deletes it.
    pub fn update_server(
        &mut self,
        key: ProjectKey,
        server: &Server,
        password: Option<&str>,
    ) -> Result<(), ModelError> {
        let secrets = self.front.secrets.clone();
        let window = self.window(key)?;
        window.project.update_server(server, secrets.as_ref())?;
        if let (Auth::Basic { .. }, Some(password)) = (&server.auth, password) {
            window
                .project
                .set_server_password(server.id, Some(password), secrets.as_ref())?;
        }
        self.servers_changed(key)
    }

    /// Settings ▸ Servers ▸ −. Requests that used it fall back to the popup's default.
    pub fn delete_server(&mut self, key: ProjectKey, server: ServerId) -> Result<(), ModelError> {
        let secrets = self.front.secrets.clone();
        self.window(key)?
            .project
            .delete_server(server, secrets.as_ref())?;
        self.servers_changed(key)
    }

    /// Settings ▸ Servers ▸ Delete Server: asks first, since the server's stored password goes
    /// with it and requests that used it fall back to the popup's default.
    pub fn ask_delete_server(
        &mut self,
        key: ProjectKey,
        server: ServerId,
    ) -> Result<(), ModelError> {
        let name = self
            .window(key)?
            .servers()
            .iter()
            .find(|s| s.id == server)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let id = DialogId(self.next());
        self.dialogs.insert(
            id,
            PendingDialog::DeleteServer {
                project: key,
                server,
            },
        );
        self.front.dialogs.confirm(
            id,
            Confirm {
                title: format!("Delete “{name}”?"),
                message: "Requests that used it will use another server. Its password is \
                          removed from the Keychain."
                    .into(),
                action: "Delete".into(),
            },
        );
        Ok(())
    }

    /// Removes the server's stored password, so that Basic auth sends an empty one.
    pub fn clear_server_password(
        &mut self,
        key: ProjectKey,
        server: ServerId,
    ) -> Result<(), ModelError> {
        let secrets = self.front.secrets.clone();
        self.window(key)?
            .project
            .set_server_password(server, None, secrets.as_ref())?;
        self.servers_changed(key)
    }

    /// For the settings form's password field.
    pub fn server_password(
        &self,
        key: ProjectKey,
        server: ServerId,
    ) -> Result<Option<String>, ModelError> {
        let window = self.project(key).ok_or(ModelError::UnknownProject)?;
        Ok(window
            .project
            .server_password(server, self.front.secrets.as_ref())?)
    }

    fn requests_changed(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let selected = window.selected_request();
        window.reload_requests()?;
        let deselected = window.selected_request() != selected;
        self.events.push(Event::SidebarChanged { project: key });
        if deselected {
            self.events.push(Event::SelectionChanged { project: key });
            self.load_editor(key);
            self.events.push(Event::EditorReplaced { project: key });
        }
        Ok(())
    }

    fn servers_changed(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        self.window(key)?.reload_servers()?;
        self.events.push(Event::ServersChanged { project: key });
        self.events
            .push(Event::ServerSelectionChanged { project: key });
        Ok(())
    }
}
