//! What changed, for the front end to apply to its widgets. Events are small and name what to
//! redraw; the front end reads the details from the [`App`](crate::App) when it applies them.

use std::ops::Range;

use washboard_core::model::RequestId;

use crate::app::ProjectKey;

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
    /// The editor shows a different request (or none); reload its whole text.
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
}
