//! The model's one-shot timers, at most one per project and kind. Restarting a kind cancels
//! the running one, which is what debouncing needs.

use std::time::Duration;

use crate::app::{App, ProjectKey};
use crate::front_end::TimerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerKind {
    /// Saves the editor (PLAN §4 "Save / autosave").
    Autosave,
}

impl App {
    /// Starts the project's `kind` timer, cancelling a running one.
    pub(crate) fn restart_timer(&mut self, key: ProjectKey, kind: TimerKind, after: Duration) {
        self.stop_timer(key, kind);
        let id = TimerId(self.next());
        self.timers.insert(id, (key, kind));
        self.front.timers.start(id, after);
    }

    pub(crate) fn stop_timer(&mut self, key: ProjectKey, kind: TimerKind) {
        self.stop_timers_where(|k, t| k == key && t == kind);
    }

    /// Stops every timer of the project.
    pub(crate) fn stop_timers(&mut self, key: ProjectKey) {
        self.stop_timers_where(|k, _| k == key);
    }

    fn stop_timers_where(&mut self, matches: impl Fn(ProjectKey, TimerKind) -> bool) {
        let ids: Vec<TimerId> = self
            .timers
            .iter()
            .filter(|(_, (k, t))| matches(*k, *t))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.timers.remove(&id);
            self.front.timers.cancel(id);
        }
    }

    /// The front end reports a timer started through [`Timers`](crate::Timers). Unknown ids
    /// (a timer cancelled after it already fired) are ignored.
    pub fn timer_fired(&mut self, id: TimerId) {
        let Some((key, kind)) = self.timers.remove(&id) else {
            return;
        };
        match kind {
            TimerKind::Autosave => {
                self.flush_or_alert(key);
            }
        }
    }
}
