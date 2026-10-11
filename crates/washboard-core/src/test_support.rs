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

/// `fixtures/<dir>/<entry>` loaded with the files and folders `extra` names in `dir`.
pub fn load_wsdl(dir: &str, entry: &str, extra: &[&str]) -> crate::wsdl::Wsdl {
    let root = fixtures().join(dir);
    let extra: Vec<PathBuf> = extra.iter().map(|e| root.join(e)).collect();
    let sources = crate::wsdl::Sources::from_disk(&root.join(entry), &extra).expect("sources");
    crate::wsdl::load(&sources)
}

/// `fixtures/customer`: two WSDLs, external schemas and an inline one.
pub fn customer() -> crate::wsdl::Wsdl {
    load_wsdl(
        "customer",
        "CustomerService.wsdl",
        &["CustomerBinding.wsdl", "xsd"],
    )
}

/// `fixtures/legacy-rpc`: one rpc/literal WSDL.
pub fn legacy() -> crate::wsdl::Wsdl {
    load_wsdl("legacy-rpc", "Legacy.wsdl", &[])
}
