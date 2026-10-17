//! Project window commands (`docs/specs/requests.md`, `docs/specs/settings.md`). Every change goes
//! straight to the project folder; the window's state is re-read from it so the sidebar always
//! shows what is on disk.

use washboard_core::model::{Auth, OperationRef, RequestId, Server, ServerId};
use washboard_core::project::names;
use washboard_core::schema::TemplateOptions;

use crate::app::{App, ModelError, PendingDialog, ProjectKey};
use crate::event::Event;
use crate::front_end::Confirm;
use crate::timers::TimerKind;

/// What a server added without a name is called, numbered from 2 when taken.
const NEW_SERVER_NAME: &str = "New Server";

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

    /// File ▸ New Request…: generates the body from the schema, names it `<Operation> <n>`,
    /// selects it and starts inline rename. It goes into `folder` (the folder selected in the
    /// sidebar), else into the operation's folder, created on first use and matched ignoring
    /// case ([`Project::create_request_for`]).
    ///
    /// [`Project::create_request_for`]: washboard_core::project::Project::create_request_for
    pub fn new_request(
        &mut self,
        key: ProjectKey,
        operation: &OperationRef,
        folder: Option<&str>,
    ) -> Result<RequestId, ModelError> {
        let opts = TemplateOptions {
            indent: " ".repeat(self.format.indent),
            ..TemplateOptions::default()
        };
        let window = self.window(key)?;
        let schema = window.schema.ready()?.clone();
        let text = schema.request_template(operation, &opts)?;
        let project = &mut window.project;
        let meta = project.create_request_for(&schema.wsdl, operation, folder, None, &text)?;
        window.expand(&meta.folder)?;
        self.requests_changed(key)?;
        self.select_request(key, Some(meta.id))?;
        self.events.push(Event::BeginRename {
            project: key,
            request: meta.id,
        });
        Ok(meta.id)
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
        self.confirm_delete(
            PendingDialog::DeleteRequest {
                project: key,
                request,
            },
            format!("Delete “{name}”?"),
            "The request and its response history are deleted. This can't be undone.",
        );
        Ok(())
    }

    /// After the confirmation. The next request (else the previous) is selected.
    pub(crate) fn delete_request_now(
        &mut self,
        key: ProjectKey,
        request: RequestId,
    ) -> Result<(), ModelError> {
        self.delete_requests(key, &[request], |project| {
            project.delete_request(request)?;
            Ok(())
        })
    }

    /// Runs `delete`, which removes `gone` from the project. If the selected request is
    /// among them, the next request in the sidebar's order (else the previous) is selected;
    /// the open editor's edits go with its request.
    fn delete_requests(
        &mut self,
        key: ProjectKey,
        gone: &[RequestId],
        delete: impl FnOnce(&mut washboard_core::project::Project) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let order = window.sidebar.request_order();
        let selected = window.selected_request();
        let was_selected = selected.is_some_and(|s| gone.contains(&s));
        let neighbour = selected
            .and_then(|s| order.iter().position(|r| *r == s))
            .and_then(|i| {
                let kept = |r: &&RequestId| !gone.contains(r);
                order[i + 1..]
                    .iter()
                    .find(kept)
                    .or_else(|| order[..i].iter().rev().find(kept))
                    .copied()
            });
        delete(&mut window.project)?;
        if window
            .editor
            .as_ref()
            .is_some_and(|e| gone.contains(&e.request()))
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

    /// File ▸ New Folder: `New Folder` (numbered when taken) in `parent`, which is shown
    /// expanded; its row then enters inline rename. Returns its path.
    pub fn new_folder(&mut self, key: ProjectKey, parent: &str) -> Result<String, ModelError> {
        let window = self.window(key)?;
        let path = window.project.create_untitled_folder(parent)?;
        window.expand(&path)?;
        self.requests_changed(key)?;
        self.events.push(Event::BeginRenameFolder {
            project: key,
            path: path.clone(),
        });
        Ok(path)
    }

    /// Inline rename of a folder; the requests in it keep their ids and history. Name errors
    /// come back like a request's. Returns the new path.
    pub fn rename_folder(
        &mut self,
        key: ProjectKey,
        path: &str,
        name: &str,
    ) -> Result<String, ModelError> {
        let window = self.window(key)?;
        let to = window.project.rename_folder(path, name)?;
        window.folder_moved(path, &to);
        window.save_collapsed()?;
        self.requests_changed(key)?;
        Ok(to)
    }

    /// Drag and drop of a folder onto another one or the top level (`""`). Refused into
    /// itself or a folder inside it, and onto a folder of the same name. Returns the new path.
    pub fn move_folder(
        &mut self,
        key: ProjectKey,
        path: &str,
        parent: &str,
    ) -> Result<String, ModelError> {
        let window = self.window(key)?;
        let to = window.project.move_folder(path, parent)?;
        window.folder_moved(path, &to);
        window.expand(parent)?;
        self.requests_changed(key)?;
        Ok(to)
    }

    /// Drag and drop of a request onto a folder or the top level (`""`). The open editor
    /// stays open: the request keeps its id.
    pub fn move_request(
        &mut self,
        key: ProjectKey,
        request: RequestId,
        folder: &str,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let meta = window.project.move_request(request, folder)?;
        window.expand(&meta.folder)?;
        self.requests_changed(key)
    }

    /// A folder's Delete. A folder with requests asks first, saying how many go, since their
    /// history goes with them; an empty one is deleted right away.
    pub fn delete_folder(&mut self, key: ProjectKey, path: &str) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let count = window
            .sidebar
            .tree
            .folder(path)
            .map_or(0, |f| f.request_count());
        if count == 0 {
            return self.delete_folder_now(key, path);
        }
        let name = names::split(path).1;
        let requests = if count == 1 {
            "its request".to_owned()
        } else {
            format!("its {count} requests")
        };
        self.confirm_delete(
            PendingDialog::DeleteFolder {
                project: key,
                path: path.to_owned(),
            },
            format!("Delete “{name}” and {requests}?"),
            "Their history is deleted too. This can't be undone.",
        );
        Ok(())
    }

    /// After the confirmation, or right away for an empty folder.
    pub(crate) fn delete_folder_now(
        &mut self,
        key: ProjectKey,
        path: &str,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let gone: Vec<RequestId> = window
            .sidebar
            .requests
            .iter()
            .filter(|r| names::is_within(&r.folder, path))
            .map(|r| r.id)
            .collect();
        self.delete_requests(key, &gone, |project| {
            project.delete_folder(path)?;
            Ok(())
        })?;
        let window = self.window(key)?;
        window.collapsed.retain(|p| !names::is_within(p, path));
        window.save_collapsed()?;
        Ok(())
    }

    /// The sidebar's disclosure triangle on a folder. Remembered per project.
    pub fn set_folder_collapsed(
        &mut self,
        key: ProjectKey,
        path: &str,
        collapsed: bool,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let changed = if collapsed {
            window.collapsed.insert(path.to_owned())
        } else {
            window.collapsed.remove(path)
        };
        if changed {
            window.save_collapsed()?;
        }
        Ok(())
    }

    /// The toolbar's server popup. Remembered on the selected request, which is also what a new
    /// request starts with (`docs/specs/requests.md`).
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

    /// Settings ▸ Servers ▸ Add Server…'s Save: adds the sheet's server in one step, so a
    /// server the project refuses (no URL, a bad one, a taken name) is never added. A blank
    /// name becomes “New Server”, numbered when taken. `password` as for
    /// [`update_server`](Self::update_server).
    pub fn add_server(
        &mut self,
        key: ProjectKey,
        server: &Server,
        password: Option<&str>,
    ) -> Result<(), ModelError> {
        let secrets = self.front.secrets.clone();
        let window = self.window(key)?;
        let mut server = server.clone();
        if server.name.trim().is_empty() {
            server.name = free_server_name(window.servers());
        }
        window.project.add_server(&server)?;
        // The server is added even if its password cannot be stored; the list shows it either
        // way, so the sheet can go on editing it.
        let stored = match (&server.auth, password) {
            (Auth::Basic { .. }, Some(password)) => {
                window
                    .project
                    .set_server_password(server.id, Some(password), secrets.as_ref())
            }
            _ => Ok(()),
        };
        self.servers_changed(key)?;
        Ok(stored?)
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

    /// Delete Server in a server's sheet (Settings ▸ Servers). Requests that used it fall back
    /// to the popup's default.
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
        self.confirm_delete(
            PendingDialog::DeleteServer {
                project: key,
                server,
            },
            format!("Delete “{name}”?"),
            "Requests that used it will use another server. Its password is removed from the \
             Keychain.",
        );
        Ok(())
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

    /// The one shape every Delete confirmation takes: a destructive "Delete" and Cancel, with
    /// `pending` saying what to delete once it is confirmed.
    fn confirm_delete(&mut self, pending: PendingDialog, title: String, message: &str) {
        let id = self.pend_dialog(pending);
        self.front.dialogs.confirm(
            id,
            Confirm {
                title,
                message: message.into(),
                action: "Delete".into(),
                cancel: None,
            },
        );
    }
}

/// “New Server”, or the first of “New Server 2”, “New Server 3”, … no server is called.
fn free_server_name(servers: &[Server]) -> String {
    let taken = |name: &str| {
        servers
            .iter()
            .any(|s| s.name.trim().to_lowercase() == name.to_lowercase())
    };
    (1..)
        .map(|n| match n {
            1 => NEW_SERVER_NAME.to_owned(),
            n => format!("{NEW_SERVER_NAME} {n}"),
        })
        .find(|name| !taken(name))
        .unwrap_or_else(|| NEW_SERVER_NAME.to_owned())
}
