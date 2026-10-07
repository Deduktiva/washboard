//! The whole pipeline over the fixtures: WSDL loading (WP-WSDL) → schema bundle → libxml2
//! compile (WP-LIBXML2) → request validation (WP-VALIDATE), checked against the expectations
//! written into the fixture requests (`fixtures/README.md`).
//!
//! The same ground truth the lxml oracle (`fixtures/check_fixtures.py`) runs on.

use std::fs;
use std::path::{Path, PathBuf};

use washboard_core::validate::{self, RequestSchema, Validation};
use washboard_core::{wsdl, xml};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Parses `<!-- expect: valid -->` / `<!-- expect: error line N: text -->`.
fn expectation(text: &str) -> Option<(u32, String)> {
    let first = text.lines().next().unwrap_or_default();
    let rest = first
        .strip_prefix("<!-- expect: ")
        .and_then(|r| r.strip_suffix(" -->"))
        .unwrap_or_else(|| panic!("missing expect comment: {first:?}"));
    if rest == "valid" {
        return None;
    }
    let (line, needle) = rest
        .strip_prefix("error line ")
        .and_then(|r| r.split_once(": "))
        .unwrap_or_else(|| panic!("malformed expect comment: {first:?}"));
    Some((line.parse().expect("line number"), needle.to_lowercase()))
}

fn errors(v: &Validation) -> Vec<(u32, &str)> {
    v.diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| (d.pos.map_or(0, |p| p.line), d.message.as_str()))
        .collect()
}

fn check_project(dir: &str, entry: &str, extras: &[&str]) {
    let root = fixtures().join(dir);
    let extra: Vec<PathBuf> = extras.iter().map(|e| root.join(e)).collect();
    let sources = wsdl::Sources::from_disk(&root.join(entry), &extra).expect("fixture sources");
    let loaded = wsdl::load(&sources);
    assert!(
        !loaded.check.has_errors(),
        "{dir}: import check failed: {:#?}",
        loaded.check
    );
    let schema = RequestSchema::compile(&loaded.bundle)
        .unwrap_or_else(|d| panic!("{dir}: bundle does not compile: {d:#?}"));

    let mut checked = 0;
    for f in fs::read_dir(root.join("requests")).expect("requests dir") {
        let path = f.expect("dir entry").path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let bytes = fs::read(&path).expect("read request");
        let text = xml::decode(&bytes).expect("decode").text;
        let v = validate::validate_request(&loaded, &schema, &text, None);
        let found = errors(&v);

        match expectation(&text) {
            None => {
                assert!(
                    found.is_empty(),
                    "{dir}/{name}: expected valid, got {found:#?}"
                );
                assert!(
                    v.operation().is_some(),
                    "{dir}/{name}: a valid request dispatches to an operation"
                );
            }
            Some((line, needle)) => assert!(
                found
                    .iter()
                    .any(|(l, m)| *l == line && m.to_lowercase().contains(needle.as_str())),
                "{dir}/{name}: expected an error on line {line} containing {needle:?}, \
                 got {found:#?}"
            ),
        }
        checked += 1;
    }
    eprintln!("{dir}: {checked} requests checked");
    assert!(checked > 0, "{dir}: no requests checked");
}

#[test]
fn customer_fixture_end_to_end() {
    check_project(
        "customer",
        "CustomerService.wsdl",
        &["CustomerBinding.wsdl", "xsd"],
    );
}

#[test]
fn legacy_rpc_fixture_end_to_end() {
    check_project("legacy-rpc", "Legacy.wsdl", &[]);
}
