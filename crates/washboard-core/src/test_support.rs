//! Helpers shared by the unit tests. Integration tests (`tests/`) cannot see `cfg(test)`
//! items and keep their own one-line copy of [`fixtures`].

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

/// The repository's `fixtures/` directory.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// A fixture file under `fixtures/`, decoded the way every XML input is (`xml::decode`).
pub fn read_fixture(rel: &str) -> String {
    let bytes = std::fs::read(fixtures().join(rel)).expect("fixture readable");
    crate::xml::decode(&bytes).expect("fixture decodes").text
}
