//! What changed, for the front end to apply to its widgets. Events are small and name what to
//! redraw; the front end reads the details from the [`App`](crate::App) when it applies them.

use std::ops::Range;

use washboard_core::model::RequestId;

use crate::app::ProjectKey;
use crate::import::ImportTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A project window should be created for `project`.
    ProjectOpened { project: ProjectKey },
    /// The window for `project` should close; the project is no longer in the app.
    ProjectClosed { project: ProjectKey },
    /// The user opened a project that is already open; bring its window forward.
    FocusProject { project: ProjectKey },
    /// The welcome window is shown exactly while no project is open.
    WelcomeVisibility { visible: bool },
    /// The welcome window's list and File ▸ Open Recent changed.
    RecentProjectsChanged,
    /// The project's sidebar rows changed (requests, markers or operations).
    SidebarChanged { project: ProjectKey },
    /// A different request (or none) is selected.
    SelectionChanged { project: ProjectKey },
    /// A new request's row should enter inline rename (PLAN §4 "Requests").
    BeginRename {
        project: ProjectKey,
        request: RequestId,
    },
    /// The server list changed; the popup and the settings sheet reload it.
    ServersChanged { project: ProjectKey },
    /// The server popup's selection changed.
    ServerSelectionChanged { project: ProjectKey },
    /// The editor shows a different request (or none), or its text was replaced by the model;
    /// reload its whole text, and the response pane and history with it.
    EditorReplaced { project: ProjectKey },
    /// Re-colour this UTF-16 range of the editor's text after an edit.
    TokensChanged {
        project: ProjectKey,
        range: Range<usize>,
    },
    /// The window's edited state ([`ProjectWindow::edited`](crate::ProjectWindow::edited))
    /// changed.
    EditedChanged { project: ProjectKey },
    /// The editor's issues (list, underlines) changed.
    DiagnosticsChanged { project: ProjectKey },
    /// A send started or ended (Send ↔ Cancel).
    SendStateChanged { project: ProjectKey },
    /// Send was refused for validation errors: show the issues list.
    ShowIssues { project: ProjectKey },
    /// The response pane shows something else.
    ResponseChanged { project: ProjectKey },
    /// The window switched between the latest exchange and an older one, or to another older
    /// one ([`ProjectWindow::older_exchange`](crate::ProjectWindow::older_exchange)).
    ShownExchangeChanged { project: ProjectKey },
    /// The selected request's history list changed.
    HistoryChanged { project: ProjectKey },
    /// An exchange was added to the HTTP log (and the oldest maybe dropped).
    LogAppended,
    /// An import sheet opened, closed or changed (check finished, fields set).
    ImportChanged { target: ImportTarget },
    /// Replace WSDL finished: schema reloaded, requests re-validated, outcome available.
    WsdlReplaced { project: ProjectKey },
}
