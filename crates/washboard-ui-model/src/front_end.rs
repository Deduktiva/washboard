//! What a front end provides. Every call is made on the main thread except
//! [`MainThread::wake`], which workers call.
//!
//! Timers and dialogs are asynchronous on every toolkit we care about (sheets on macOS), so
//! the model never waits for them: it hands out an id, and the front end reports back with
//! [`App::timer_fired`](crate::App::timer_fired) or
//! [`App::dialog_answered`](crate::App::dialog_answered).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use washboard_core::secrets::SecretStore;

/// Lets workers get the model's attention.
pub trait MainThread: Send + Sync {
    /// Asks the front end to call [`App::pump`](crate::App::pump) on the main thread soon.
    /// Called from worker threads; several wakes may be coalesced into one pump.
    fn wake(&self);
}

/// Identifies a timer the model started; the front end echoes it back when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(pub(crate) u64);

/// One-shot timers for autosave and debouncing.
pub trait Timers {
    /// Starts `id` to fire once after `after`. Starting an id that is already running restarts
    /// it, which is how debouncing works.
    fn start(&self, id: TimerId, after: Duration);
    /// Stops `id` if it is running. A timer that already fired is not reported again.
    fn cancel(&self, id: TimerId);
}

/// Identifies a dialog the model asked for; the front end echoes it back with the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DialogId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAnswer {
    Folder(PathBuf),
    Cancelled,
}

/// A message the user only acknowledges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub message: String,
}

/// Panels and alerts. Answers come back through
/// [`App::dialog_answered`](crate::App::dialog_answered).
pub trait Dialogs {
    /// An open panel for a project folder.
    fn choose_project_folder(&self, id: DialogId);
    /// Shown without waiting for an answer.
    fn alert(&self, alert: Alert);
}

/// The front end's implementations, handed to [`App::new`](crate::App::new).
pub struct FrontEnd {
    pub main_thread: Arc<dyn MainThread>,
    pub timers: Box<dyn Timers>,
    pub dialogs: Box<dyn Dialogs>,
    pub secrets: Arc<dyn SecretStore>,
}

impl fmt::Debug for FrontEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrontEnd").finish_non_exhaustive()
    }
}
