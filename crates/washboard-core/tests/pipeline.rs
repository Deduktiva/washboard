//! Cross-package check: WSDL loading (WP-WSDL) → schema bundle → libxml2 compile and
//! validate (WP-LIBXML2), against the expectations written into the fixture requests.
//!
//! This is a stand-in until WP-VALIDATE builds the real pipeline. It covers the schema-level
//! fixtures; well-formedness and SOAP envelope checks belong to WP-VALIDATE.

use std::fs;
use std::path::{Path, PathBuf};

use washboard_core::model::QName;
use washboard_core::soap::SOAP11_ENV_NS;
use washboard_core::validate::xsd::CompiledSchema;
use washboard_core::{wsdl, xml};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Parses `<!-- expect: valid -->` / `<!-- expect: error line N: text -->`.
fn expectation(text: &str) -> Option<(u32, String)> {
    let first = text.lines().next().unwrap_or_default();
    let rest = first.strip_prefix("<!-- expect: ")?.strip_suffix(" -->")?;
    if rest == "valid" {
        return None;
    }
    let rest = rest.strip_prefix("error line ")?;
    let (line, needle) = rest.split_once(": ")?;
    Some((line.parse().ok()?, needle.to_lowercase()))
}

/// Cuts `node` out as a standalone document whose lines match the original file: the text is
/// prefixed with newlines, and the in-scope namespace declarations are added to its start tag.
fn standalone(text: &str, node: roxmltree::Node<'_, '_>) -> String {
    let range = node.range();
    let line = text[..range.start].matches('\n').count();
    let mut decls = String::new();
    for ns in node.namespaces() {
        match ns.name() {
            Some(p) => decls.push_str(&format!(" xmlns:{p}=\"{}\"", ns.uri())),
            None => decls.push_str(&format!(" xmlns=\"{}\"", ns.uri())),
        }
    }
    let elem = &text[range];
    let name_end = elem
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(elem.len());
    format!(
        "{}{}{decls}{}",
        "\n".repeat(line),
        &elem[..name_end],
        &elem[name_end..]
    )
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
    let schema = CompiledSchema::compile(&loaded.bundle)
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
        let expect = expectation(&text);
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            continue; // not well-formed: WP-VALIDATE
        };
        let env = doc.root_element();
        if env.tag_name().namespace() != Some(SOAP11_ENV_NS) {
            continue; // SOAP 1.2 envelope: WP-VALIDATE
        }

        let mut errors = Vec::new();
        let mut blocks = Vec::new();
        for part in env.children().filter(|n| n.is_element()) {
            let is_body = part.tag_name().name() == "Body";
            for child in part.children().filter(|n| n.is_element()) {
                if is_body {
                    let qn = QName::new(
                        child.tag_name().namespace().unwrap_or_default(),
                        child.tag_name().name(),
                    );
                    if loaded.dispatch(&qn, None).is_none() {
                        let line = text[..child.range().start].matches('\n').count() as u32 + 1;
                        errors.push((line, format!("no operation for {qn}")));
                        continue;
                    }
                }
                blocks.push(child);
            }
        }
        for block in blocks {
            for d in schema.validate(standalone(&text, block).as_bytes()) {
                errors.push((d.pos.map_or(0, |p| p.line), d.message));
            }
        }

        match &expect {
            None => assert!(
                errors.is_empty(),
                "{dir}/{name}: expected valid, got {errors:#?}"
            ),
            Some((line, needle)) => assert!(
                errors
                    .iter()
                    .any(|(l, m)| l == line && m.to_lowercase().contains(needle.as_str())),
                "{dir}/{name}: expected error on line {line} containing {needle:?}, got {errors:#?}"
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
