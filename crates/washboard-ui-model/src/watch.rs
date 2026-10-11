//! Request files changed outside the app (`docs/specs/editor.md`; `notify`, FSEvents on macOS): an
//! editor, `git checkout`, Finder.
//!
//! The watcher only says that something under `requests/` changed. After a short debounce the
//! model re-reads the folder ([`Project::reconcile`](washboard_core::project::Project)) and the
//! open request's file, so a burst of events (an atomic save, a checkout) is one rescan, and the
//! model's own saves are told apart by content rather than by timing: a file whose text is what
//! the model last read or wrote has not changed.
//!
//! The open request, when its file changed:
//! - without unsaved edits, it is reloaded silently;
//! - with unsaved edits, the user is asked: Reload discards the edits, Keep Edits saves them
//!   over the file. Nothing is saved while the question is open, and autosave checks the file
//!   before every write, so an outside change is never overwritten unasked, even one the
//!   watcher has not reported yet.
//!
//! A request whose file is gone leaves the sidebar, unsaved edits included, as when it is
//! deleted in the app.
//!
//! Watching is off until the front end turns it on ([`App::watch_request_files`]), so model
//! tests see no events from their own writes unless they ask for them. It is macOS-only; see
//! the other `backend`.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use washboard_core::model::RequestId;
use washboard_core::project::REQUESTS_DIR;

use crate::app::{App, PendingDialog, ProjectKey};
use crate::editor::fingerprint;
use crate::event::Event;
use crate::front_end::{Confirm, DialogAnswer};
use crate::timers::TimerKind;

/// How long the folder must be quiet before it is re-read.
pub(crate) const RESCAN_DELAY: Duration = Duration::from_millis(200);

/// The app's side of the watchers: whether to watch, and the channel their threads report on.
#[derive(Debug)]
pub(crate) struct Watching {
    enabled: bool,
    tx: Sender<ProjectKey>,
    rx: Receiver<ProjectKey>,
}

impl Watching {
    pub(crate) fn new() -> Watching {
        let (tx, rx) = channel();
        Watching {
            enabled: false,
            tx,
            rx,
        }
    }
}

pub(crate) use backend::RequestWatcher;

#[cfg(target_os = "macos")]
mod backend {
    use std::fmt;
    use std::path::Path;

    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    /// Watches one project's `requests/` until dropped, with the project's window.
    pub(crate) struct RequestWatcher {
        _watcher: RecommendedWatcher,
    }

    impl fmt::Debug for RequestWatcher {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("RequestWatcher")
        }
    }

    /// Calls `on_change` on the watcher's thread whenever something under `dir` changed.
    pub(crate) fn watch(
        dir: &Path,
        on_change: impl Fn() + Send + 'static,
    ) -> Option<RequestWatcher> {
        let mut watcher = notify::recommended_watcher(move |result| {
            if worth_rescan(&result) {
                on_change();
            }
        })
        .ok()?;
        watcher.watch(dir, RecursiveMode::Recursive).ok()?;
        Some(RequestWatcher { _watcher: watcher })
    }

    /// Worth a rescan: anything but reads, unless it only concerns hidden files such as the
    /// temporary file of an atomic save. Errors count, since a watcher that lost events can
    /// only catch up by re-reading.
    fn worth_rescan(result: &notify::Result<notify::Event>) -> bool {
        let Ok(event) = result else {
            return true;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        event.paths.is_empty()
            || event.paths.iter().any(|p| {
                p.file_name()
                    .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
            })
    }
}

/// No watching off macOS: `notify`'s inotify backend brings `mio`, which the network boundary check
/// keeps out of the dependency graph (`docs/specs/network.md`), and only macOS has a front end. The
/// rescan itself is the same everywhere and tested on Linux.
#[cfg(not(target_os = "macos"))]
mod backend {
    use std::path::Path;

    #[derive(Debug)]
    pub(crate) enum RequestWatcher {}

    pub(crate) fn watch(
        _dir: &Path,
        _on_change: impl Fn() + Send + 'static,
    ) -> Option<RequestWatcher> {
        None
    }
}

impl App {
    /// Watches every open project's `requests/` from now on, and those opened later. A front end
    /// calls it once, before [`launch`](Self::launch).
    pub fn watch_request_files(&mut self) {
        self.watching.enabled = true;
        let keys: Vec<_> = self
            .projects
            .iter()
            .filter(|(_, w)| w.watcher.is_none())
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            self.start_watcher(key);
        }
    }

    /// Starts the project's watcher, if watching is on. A watcher that can't start leaves the
    /// project unwatched: autosave still checks the file
    /// before writing, so nothing is overwritten, changes just aren't shown until reopened.
    pub(crate) fn start_watcher(&mut self, key: ProjectKey) {
        if !self.watching.enabled {
            return;
        }
        let tx = self.watching.tx.clone();
        let main = self.front.main_thread.clone();
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let dir = window.project.root().join(REQUESTS_DIR);
        window.watcher = backend::watch(&dir, move || {
            // The app is gone if the send fails; nothing is left to tell.
            if tx.send(key).is_ok() {
                main.wake();
            }
        });
    }

    /// From [`pump`](Self::pump): (re)starts the debounce of every project whose files changed.
    pub(crate) fn note_watched_changes(&mut self) {
        while let Ok(key) = self.watching.rx.try_recv() {
            if self.window_mut(key).is_some() {
                self.restart_timer(key, TimerKind::Rescan, RESCAN_DELAY);
            }
        }
    }

    /// Brings the sidebar and the open request up to date with `requests/`.
    pub(crate) fn rescan(&mut self, key: ProjectKey) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        // A folder that can't be read now fails the next command too, which reports it.
        if window.project.reconcile().is_err() {
            return;
        }
        let before = window.sidebar.clone();
        let selected = window.selected_request();
        if window.reload_requests().is_err() {
            return;
        }
        if window.sidebar != before {
            self.events.push(Event::SidebarChanged { project: key });
        }
        let Some(window) = self.window_mut(key) else {
            return;
        };
        if window.selected_request() != selected {
            // The open request's file is gone.
            self.events.push(Event::SelectionChanged { project: key });
            self.load_editor(key);
            self.events.push(Event::EditorReplaced { project: key });
            self.events.push(Event::EditedChanged { project: key });
            return;
        }

        let Some(editor) = &window.editor else {
            return;
        };
        // Unreadable for now (mid-write by a tool that doesn't save atomically): the write's own
        // event brings another rescan.
        let Ok(text) = window.project.read_request(editor.request()) else {
            return;
        };
        if fingerprint(&text) == editor.disk {
            return;
        }
        if editor.dirty() {
            self.ask_reload(key);
        } else {
            self.reload_editor(key);
        }
    }

    /// Asks whether to reload the open request from disk or keep its edits, unless that is
    /// already asked. Autosave waits for the answer.
    pub(crate) fn ask_reload(&mut self, key: ProjectKey) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let Some(editor) = window.editor.as_mut().filter(|e| !e.asking) else {
            return;
        };
        editor.asking = true;
        let request = editor.request();
        let name = window
            .sidebar
            .request(request)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        self.stop_timer(key, TimerKind::Autosave);
        let id = self.pend_dialog(PendingDialog::ReloadRequest {
            project: key,
            request,
        });
        self.front.dialogs.confirm(
            id,
            Confirm {
                title: format!("“{name}” was changed by another app"),
                message: "It has unsaved edits in Washboard. Reload it to discard them, or \
                          keep your edits and save them over the other change."
                    .into(),
                action: "Reload".into(),
                cancel: Some("Keep Edits".into()),
            },
        );
    }

    /// The answer to [`ask_reload`](Self::ask_reload). Ignored if the request is no longer
    /// open with the question pending.
    pub(crate) fn reload_answered(
        &mut self,
        key: ProjectKey,
        request: RequestId,
        answer: DialogAnswer,
    ) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let Some(editor) = window
            .editor
            .as_mut()
            .filter(|e| e.asking && e.request() == request)
        else {
            return;
        };
        editor.asking = false;
        match answer {
            DialogAnswer::Confirmed => self.reload_editor(key),
            _ => {
                // What is on disk now is what the edits replace; a file that can't be read
                // is simply written.
                if let Ok(text) = window.project.read_request(request) {
                    editor.disk = fingerprint(&text);
                }
                self.flush_or_alert(key);
            }
        }
    }

    /// Replaces the open request's text with the file's, dropping unsaved edits.
    fn reload_editor(&mut self, key: ProjectKey) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let was_dirty = window.edited();
        if let Some(request) = window.selected_request() {
            window.mark_dirty(request, false);
        }
        self.load_editor(key);
        self.events.push(Event::EditorReplaced { project: key });
        if was_dirty {
            self.events.push(Event::SidebarChanged { project: key });
            self.events.push(Event::EditedChanged { project: key });
        }
    }
}
