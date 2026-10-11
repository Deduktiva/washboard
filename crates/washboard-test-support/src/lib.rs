//! Helpers shared by the tests of several crates: the path to `fixtures/` and a local HTTP
//! server for the send tests.
//!
//! Every crate takes this as a dev-dependency only, so none of it ships;
//! `washboard-core/tests/network_boundary.rs` checks that, which is what lets [`server`] use
//! sockets outside `washboard_core::http` (PLAN §6). It depends on no workspace crate, so
//! core's unit tests can use it too without seeing a second copy of core's types.

pub mod server;

use std::path::{Path, PathBuf};

/// The repository's `fixtures/` directory.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
