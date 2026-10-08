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
mod event;
mod front_end;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use app::{App, ModelError, ProjectKey, ProjectWindow};
pub use event::Event;
pub use front_end::{
    Alert, DialogAnswer, DialogId, Dialogs, FrontEnd, MainThread, TimerId, Timers,
};
