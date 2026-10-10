//! Washboard macOS app (AppKit via objc2): WP-APP-SHELL and WP-APP-INTEGRATION in
//! `docs/TASKS.md`.
//!
//! A library rather than code in `main.rs` so that `tests/appkit.rs` can build the real
//! delegates, windows and views headless on the macOS CI runner. Behaviour belongs in
//! `washboard-ui-model` (PLAN §2.1); this crate draws state and forwards input.
//!
//! Cloud agents run on Linux: type-check with
//! `cargo clippy -p washboard-app --target aarch64-apple-darwin`; CI builds and tests it on
//! macOS. On other platforms the library is empty.

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod app_settings;
#[cfg(target_os = "macos")]
mod editor;
#[cfg(target_os = "macos")]
mod form;
#[cfg(target_os = "macos")]
mod front_end;
#[cfg(target_os = "macos")]
mod http_log;
#[cfg(target_os = "macos")]
mod layout;
#[cfg(target_os = "macos")]
mod menu;
#[cfg(target_os = "macos")]
mod panes;
#[cfg(target_os = "macos")]
mod project_window;
#[cfg(target_os = "macos")]
mod settings_window;
#[cfg(target_os = "macos")]
mod sheets;
#[cfg(target_os = "macos")]
mod sidebar;
#[cfg(target_os = "macos")]
mod table;
#[cfg(target_os = "macos")]
mod welcome;
// Used by the views; built everywhere so its tests run on Linux.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod text;

#[cfg(target_os = "macos")]
pub use app::{AppDelegate, Options, install, run};
#[cfg(target_os = "macos")]
pub use app_settings::{INDENT_KEY, ON_SAVE_KEY};
#[cfg(target_os = "macos")]
pub use editor::{EditorController, LineNumberRuler};
#[cfg(target_os = "macos")]
pub use form::{GROUP_ID, HEADER_ID, MAX_WIDTH, PAGE_MARGIN, ROW_INSET, TEXT_ID};
#[cfg(target_os = "macos")]
pub use http_log::HttpLog;
#[cfg(target_os = "macos")]
pub use panes::{IssuesBar, RESPONSE_TABS, RequestBar, ResponsePane};
#[cfg(target_os = "macos")]
pub use project_window::{ProjectWindowController, project_tabbing_id, toolbar_identifiers};
#[cfg(target_os = "macos")]
pub use settings_window::{PANE_KEY, Pane, PaneItem, ServersPane, SettingsWindowController};
#[cfg(target_os = "macos")]
pub use sheets::ImportSheetController;
#[cfg(target_os = "macos")]
pub use sidebar::{NodeKind, SidebarController, SidebarNode};
#[cfg(target_os = "macos")]
pub use table::TextTable;
#[cfg(target_os = "macos")]
pub use welcome::{RecentProject, WelcomeController};
