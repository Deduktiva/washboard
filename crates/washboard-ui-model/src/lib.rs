//! The app without the toolkit (PLAN §2.1): everything Washboard *does*, as opposed to how it
//! looks. A front end (AppKit today) draws the state kept here and forwards user input; it
//! implements [`MainThread`], [`Timers`] and [`Dialogs`] and applies the [`Event`]s it drains
//! from [`App::take_events`].
//!
//! All state lives on the main thread and is not `Send`. Background work gets owned inputs and
//! hands its result back through [`App::spawn`]; the front end's [`MainThread::wake`] makes it
//! call [`App::pump`], which runs the completions on the main thread.
//!
//! Owned by WP-UI-MODEL (`docs/TASKS.md`).

mod app;
mod assist;
mod commands;
mod diagnostics;
mod editor;
mod event;
mod format;
mod front_end;
mod import;
mod send;
mod timers;
mod window;

#[cfg(test)]
mod fake;
#[cfg(test)]
#[path = "../tests/support/server.rs"]
mod server;
#[cfg(test)]
mod tests;

pub use app::{App, ModelError, ProjectKey};
pub use assist::{CompletionItem, CompletionKind, Completions, Hover};
pub use diagnostics::{Issue, IssuesBasis, WellFormedness};
pub use editor::Editor;
pub use event::Event;
pub use format::{FormatSettings, INDENT_RANGE, Reformat};
pub use front_end::{
    Alert, Confirm, DialogAnswer, DialogId, Dialogs, FrontEnd, MainThread, TimerId, Timers,
};
pub use import::{CheckState, CheckedImport, ImportSheet, ImportTarget, ReplaceOutcome};
pub use send::{HistoryDrawer, LOG_CAPACITY, LogEntry, OlderExchange, ResponseView};
pub use window::{
    OperationNode, PortNode, ProjectSchema, ProjectWindow, RequestRow, RequestSummary, SchemaState,
    ServiceNode, Sidebar, WsdlServer,
};
