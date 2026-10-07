//! Tests for WP-WSDL: the fixtures (`fixtures/customer`, `fixtures/legacy-rpc`) plus small
//! in-memory WSDL sets for edge cases. Bundle contents are checked structurally; compiling
//! the bundle with libxml2 is WP-LIBXML2's job.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use super::*;
use crate::diag::Severity;
use crate::model::SchemaOrigin;
use crate::soap::{WSDL_NS, WSDL_SOAP11_NS, XSD_NS};

const CUS_SVC: &str = "urn:example:customer:service";
const CUS_MSG: &str = "urn:example:customer:messages";
const LEGACY: &str = "urn:example:legacy";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn customer() -> Wsdl {
    let dir = fixtures().join("customer");
    let s = Sources::from_disk(
        &dir.join("CustomerService.wsdl"),
        std::slice::from_ref(&dir),
    )
    .unwrap();
    load(&s)
}

fn legacy() -> Wsdl {
    let dir = fixtures().join("legacy-rpc");
    let s = Sources::from_disk(&dir.join("Legacy.wsdl"), &[]).unwrap();
    load(&s)
}

fn q(ns: &str, local: &str) -> QName {
    QName::new(ns, local)
}

fn print_diags(w: &Wsdl) {
    for d in &w.check.diagnostics {
        eprintln!("{d}");
    }
}

/// Loads an in-memory set; the first file is the entry.
fn mem(files: &[(&str, &str)]) -> Wsdl {
    let mut it = files
        .iter()
        .map(|(p, t)| SourceFile::new(*p, t.as_bytes().to_vec()));
    let entry = it.next().unwrap();
    let w = load(&Sources::new(entry, it));
    print_diags(&w);
    w
}

/// A WSDL with the usual prefixes, `tns` = `urn:t`.
fn wsdl(types: &str, rest: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<wsdl:definitions xmlns:wsdl="{WSDL_NS}" xmlns:soap="{WSDL_SOAP11_NS}" xmlns:xs="{XSD_NS}"
    xmlns:tns="urn:t" targetNamespace="urn:t">
  <wsdl:types>{types}</wsdl:types>
{rest}
</wsdl:definitions>
"#
    )
}

fn xsd(tns: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="{XSD_NS}" targetNamespace="{tns}">
{body}
</xs:schema>
"#
    )
}

fn errors(w: &Wsdl) -> Vec<&str> {
    w.check
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect()
}

fn warnings(w: &Wsdl) -> Vec<&str> {
    w.check
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message.as_str())
        .collect()
}

/// Every `schemaLocation` in the bundle must name a bundle document.
fn assert_closed(b: &crate::model::SchemaBundle) {
    for d in &b.docs {
        for (i, _) in d.text.match_indices("schemaLocation=\"") {
            let rest = &d.text[i + 16..];
            let loc = &rest[..rest.find('"').unwrap()];
            assert!(b.get(loc).is_some(), "{} references {loc}", d.uri);
        }
    }
    assert!(b.get(&b.root).is_some());
}

/// With `WASHBOARD_DUMP_BUNDLE=<dir>` set, writes each fixture bundle as
/// `<dir>/<fixture>/manifest.tsv` (uri, target namespace, file) plus the documents, for
/// checking with an external validator (e.g. lxml with a resolver). No-op otherwise.
#[test]
fn dump_bundles_when_requested() {
    let Some(dir) = std::env::var_os("WASHBOARD_DUMP_BUNDLE") else {
        return;
    };
    for (name, w) in [("customer", customer()), ("legacy-rpc", legacy())] {
        let out = Path::new(&dir).join(name);
        std::fs::create_dir_all(&out).unwrap();
        let mut manifest = format!("root\t{}\n", w.bundle.root);
        for (i, d) in w.bundle.docs.iter().enumerate() {
            let file = format!("{i}.xsd");
            std::fs::write(out.join(&file), &d.text).unwrap();
            manifest.push_str(&format!("{}\t{}\t{file}\n", d.uri, d.target_ns));
        }
        std::fs::write(out.join("manifest.tsv"), manifest).unwrap();
    }
}

// ---------------------------------------------------------------------------------------
// Fixtures

#[test]
fn customer_loads_without_errors() {
    let w = customer();
    print_diags(&w);
    assert!(!w.check.has_errors());
}

#[test]
fn customer_operations_headers_and_soap_actions() {
    let w = customer();
    let d = &w.definitions;
    let b = d.binding(&q(CUS_SVC, "CustomerBinding")).unwrap();
    assert_eq!(b.protocol, Protocol::Soap11);
    assert_eq!(b.style, Style::Document);
    let names: Vec<&str> = b.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["GetCustomer", "CreateOrder"]);

    let get = &b.operations[0];
    assert!(get.is_supported());
    assert_eq!(
        get.soap_action.as_deref(),
        Some("urn:example:customer:service/GetCustomer")
    );
    let input = get.input.as_ref().unwrap();
    assert_eq!(input.usage, Use::Literal);
    assert_eq!(input.headers.len(), 1);
    assert_eq!(input.headers[0].message, q(CUS_SVC, "RequestContextHeader"));
    assert_eq!(input.headers[0].part, "header");
    assert_eq!(
        input.headers[0].content,
        PartContent::Element(q(CUS_MSG, "RequestContext"))
    );
    assert_eq!(
        get.body_elements(Direction::Input),
        [q(CUS_MSG, "GetCustomer")]
    );
    assert_eq!(
        get.body_elements(Direction::Output),
        [q(CUS_MSG, "GetCustomerResponse")]
    );
    assert_eq!(get.faults.len(), 1);
    assert_eq!(get.faults[0].name, "CustomerFault");
    assert_eq!(
        get.faults[0].parts[0].content,
        PartContent::Element(q(CUS_MSG, "CustomerFault"))
    );

    let create = &b.operations[1];
    assert!(create.is_supported());
    assert_eq!(create.soap_action, None);
    assert!(create.input.as_ref().unwrap().headers.is_empty());

    // SOAP 1.2 binding: listed, marked, not dropped, not an error.
    let b12 = d.binding(&q(CUS_SVC, "CustomerBinding12")).unwrap();
    assert_eq!(b12.protocol, Protocol::Soap12);
    assert_eq!(b12.operations.len(), 1);
    assert_eq!(
        b12.operations[0].support,
        Support::Unsupported(UnsupportedReason::Soap12)
    );
    assert_eq!(
        b12.operations[0].soap_action.as_deref(),
        Some("urn:example:customer:service/GetCustomer")
    );
    assert!(
        warnings(&w)
            .iter()
            .any(|m| m.contains("CustomerBinding12") && m.contains("SOAP 1.2"))
    );

    // Services and ports come from the entry file, bindings from the imported one.
    assert_eq!(d.services.len(), 1);
    let ports = &d.services[0].ports;
    assert_eq!(ports.len(), 2);
    assert_eq!(ports[0].name, "CustomerPort");
    assert_eq!(ports[0].binding, q(CUS_SVC, "CustomerBinding"));
    assert_eq!(
        ports[0].address.as_deref(),
        Some("https://customer.example.invalid/ws/customer")
    );
    assert_eq!(
        ports[1].address.as_deref(),
        Some("https://customer.example.invalid/ws/customer12")
    );
}

#[test]
fn customer_dispatch() {
    let w = customer();
    let get = w.dispatch(&q(CUS_MSG, "GetCustomer"), None).unwrap();
    assert_eq!(
        get.operation,
        OperationRef {
            binding: q(CUS_SVC, "CustomerBinding"),
            operation: "GetCustomer".into()
        }
    );
    assert_eq!(get.style, Style::Document);
    assert_eq!(
        get.soap_action.as_deref(),
        Some("urn:example:customer:service/GetCustomer")
    );
    assert_eq!(get.header_elements, [q(CUS_MSG, "RequestContext")]);
    // Only the SOAP 1.1 binding dispatches; the 1.2 one is unsupported.
    assert_eq!(w.dispatch_all(&q(CUS_MSG, "GetCustomer")).len(), 1);

    let create = w.dispatch(&q(CUS_MSG, "CreateOrder"), None).unwrap();
    assert_eq!(create.soap_action, None);
    assert!(create.header_elements.is_empty());

    // invalid-unknown-operation.xml uses msg:DeleteCustomer.
    assert!(w.dispatch(&q(CUS_MSG, "DeleteCustomer"), None).is_none());
    // Responses are not request body elements.
    assert!(
        w.dispatch(&q(CUS_MSG, "GetCustomerResponse"), None)
            .is_none()
    );

    let (b, op) = w.operation(&get.operation).unwrap();
    assert_eq!(b.name.local, "CustomerBinding");
    assert_eq!(op.name, "GetCustomer");
}

#[test]
fn customer_import_check() {
    let w = customer();
    let c = &w.check;
    let summary: Vec<(RefKind, &str, Option<&str>)> = c
        .references
        .iter()
        .map(|r| {
            let to = match &r.resolution {
                Resolution::Resolved { file, matched_by } => {
                    assert_eq!(*matched_by, MatchedBy::Path);
                    Some(file.as_str())
                }
                _ => None,
            };
            (r.kind, r.from.as_str(), to)
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                RefKind::WsdlImport,
                "CustomerService.wsdl",
                Some("CustomerBinding.wsdl")
            ),
            (
                RefKind::XsdImport,
                "CustomerBinding.wsdl",
                Some("xsd/customer.xsd")
            ),
            (
                RefKind::XsdImport,
                "xsd/customer.xsd",
                Some("xsd/common/party.xsd")
            ),
            (
                RefKind::XsdImport,
                "xsd/customer.xsd",
                Some("xsd/ext/audit.xsd")
            ),
            (
                RefKind::XsdInclude,
                "xsd/common/party.xsd",
                Some("xsd/common/party-ids.xsd")
            ),
        ]
    );
    let inline_ref = &c.references[1];
    assert_eq!(inline_ref.inline_schema, Some(0));
    assert_eq!(inline_ref.pos.line, 15);
    assert_eq!(inline_ref.pos.column, 7);
    assert_eq!(
        inline_ref.namespace.as_deref(),
        Some("urn:example:customer")
    );
    assert!(c.split_namespaces.is_empty());
    assert!(c.xsd11.is_empty());

    let info = |p: &str| c.files.iter().find(|f| f.path == p).unwrap();
    assert!(info("CustomerService.wsdl").had_bom);
    assert!(info("xsd/common/party.xsd").had_bom);
    assert!(!info("xsd/customer.xsd").had_bom);
    assert_eq!(info("CustomerService.wsdl").depth, 0);
    assert_eq!(info("xsd/common/party-ids.xsd").depth, 4);
    assert_eq!(info("CustomerBinding.wsdl").kind, FileKind::Wsdl);
    assert_eq!(info("xsd/ext/audit.xsd").kind, FileKind::Xsd);
    assert_eq!(
        info("xsd/ext/audit.xsd").target_namespace.as_deref(),
        Some("urn:example:audit")
    );
    assert!(c.files.iter().all(|f| f.used));
    assert_eq!(w.entry_dest(), "CustomerService.wsdl");
    let dests: Vec<&str> = w.layout.iter().map(|l| l.dest.as_str()).collect();
    assert!(dests.contains(&"xsd/common/party-ids.xsd"));
    assert!(
        w.layout[0]
            .source
            .ends_with("/fixtures/customer/CustomerService.wsdl")
    );
}

#[test]
fn customer_bundle() {
    let w = customer();
    let b = &w.bundle;
    let uris: Vec<&str> = b.docs.iter().map(|d| d.uri.as_str()).collect();
    assert_eq!(
        uris,
        [
            "washboard:/inline/0.xsd",
            "washboard:/wsdl/xsd/customer.xsd",
            "washboard:/wsdl/xsd/common/party.xsd",
            "washboard:/wsdl/xsd/ext/audit.xsd",
            "washboard:/wsdl/xsd/common/party-ids.xsd",
            ROOT_URI,
        ]
    );
    assert_eq!(b.root, ROOT_URI);
    assert_closed(b);

    let inline = b.get("washboard:/inline/0.xsd").unwrap();
    assert_eq!(inline.target_ns, CUS_MSG);
    assert_eq!(
        inline.origin,
        SchemaOrigin::InlineWsdl {
            wsdl_path: "CustomerBinding.wsdl".into(),
            index: 0
        }
    );
    // Namespace carry-over: prefixes declared only on wsdl:definitions.
    assert!(inline.text.contains(r#"xmlns:cus="urn:example:customer""#));
    assert!(
        inline
            .text
            .contains(r#"xmlns:msg="urn:example:customer:messages""#)
    );
    assert_eq!(inline.text.matches("xmlns:xs=").count(), 1);
    // Line numbers inside the excerpt equal those in the WSDL.
    let line_of = |t: &str, needle: &str| t[..t.find(needle).unwrap()].matches('\n').count() + 1;
    let wsdl_text =
        std::fs::read_to_string(fixtures().join("customer/CustomerBinding.wsdl")).unwrap();
    for needle in ["<xs:element name=\"GetCustomer\">", "<xs:schema "] {
        assert_eq!(
            line_of(&inline.text, needle),
            line_of(&wsdl_text, needle),
            "{needle}"
        );
    }
    assert!(
        inline
            .text
            .contains(r#"schemaLocation="washboard:/wsdl/xsd/customer.xsd""#)
    );
    assert!(inline.text.trim_end().ends_with("</xs:schema>"));

    let party = b.get("washboard:/wsdl/xsd/common/party.xsd").unwrap();
    assert_eq!(
        party.origin,
        SchemaOrigin::File {
            path: "xsd/common/party.xsd".into()
        }
    );
    assert_eq!(party.target_ns, "urn:example:common");
    assert!(!party.text.starts_with('\u{feff}'));
    assert!(
        party
            .text
            .contains(r#"<xs:include schemaLocation="washboard:/wsdl/xsd/common/party-ids.xsd"/>"#)
    );

    // The root imports each namespace exactly once; party-ids (included) is not imported.
    let root = b.get(ROOT_URI).unwrap();
    assert_eq!(root.origin, SchemaOrigin::Generated);
    assert_eq!(root.target_ns, ROOT_NS);
    assert_eq!(root.text.matches("<xs:import").count(), 4);
    for ns in [
        CUS_MSG,
        "urn:example:customer",
        "urn:example:common",
        "urn:example:audit",
    ] {
        assert_eq!(
            root.text.matches(&format!("namespace=\"{ns}\"")).count(),
            1,
            "{ns}"
        );
    }
    assert!(!root.text.contains("party-ids"));
}

#[test]
fn legacy_rpc_and_encoded() {
    let w = legacy();
    print_diags(&w);
    assert!(!w.check.has_errors());
    let d = &w.definitions;
    let lit = d.binding(&q(LEGACY, "LegacyBinding")).unwrap();
    let op = &lit.operations[0];
    assert_eq!(op.style, Style::Rpc);
    assert!(op.is_supported());
    assert_eq!(op.soap_action.as_deref(), Some("urn:example:legacy#Lookup"));
    let input = op.input.as_ref().unwrap();
    assert_eq!(input.namespace.as_deref(), Some(LEGACY));
    let parts: Vec<&str> = input.body_parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(parts, ["customerNo", "asOf"]);
    assert_eq!(
        input.body_parts[1].content,
        PartContent::Type(q(XSD_NS, "date"))
    );
    assert_eq!(op.body_elements(Direction::Input), [q(LEGACY, "Lookup")]);
    assert_eq!(
        op.body_elements(Direction::Output),
        [q(LEGACY, "LookupResponse")]
    );

    let enc = d.binding(&q(LEGACY, "LegacyEncodedBinding")).unwrap();
    assert_eq!(
        enc.operations[0].support,
        Support::Unsupported(UnsupportedReason::Encoded)
    );
    assert!(
        warnings(&w)
            .iter()
            .any(|m| m.contains("LegacyEncodedBinding") && m.contains("encoded"))
    );

    let dsp = w.dispatch_all(&q(LEGACY, "Lookup"));
    assert_eq!(dsp.len(), 1);
    assert_eq!(dsp[0].operation.binding, q(LEGACY, "LegacyBinding"));
    assert_eq!(dsp[0].style, Style::Rpc);
    assert_eq!(d.services[0].ports.len(), 2);
}

#[test]
fn legacy_rpc_bundle_includes_real_schema() {
    let w = legacy();
    let b = &w.bundle;
    assert_closed(b);
    let uris: Vec<&str> = b.docs.iter().map(|d| d.uri.as_str()).collect();
    assert_eq!(
        uris,
        ["washboard:/inline/0.xsd", "washboard:/rpc/0.xsd", ROOT_URI]
    );
    let rpc = b.get("washboard:/rpc/0.xsd").unwrap();
    assert_eq!(rpc.target_ns, LEGACY);
    assert_eq!(rpc.origin, SchemaOrigin::Generated);
    // §5.4 gotcha: same namespace as the inline schema, so include it instead of importing.
    assert!(
        rpc.text
            .contains(r#"<xs:include schemaLocation="washboard:/inline/0.xsd"/>"#)
    );
    assert!(!rpc.text.contains("<xs:import"));
    // Parts are unqualified.
    assert!(!rpc.text.contains("elementFormDefault"));
    let lookup = rpc.text.find(r#"<xs:element name="Lookup">"#).unwrap();
    let c = rpc.text[lookup..]
        .find(r#"name="customerNo" type="xs:string""#)
        .unwrap();
    let a = rpc.text[lookup..]
        .find(r#"name="asOf" type="xs:date""#)
        .unwrap();
    assert!(c < a, "parts in message order");
    assert!(rpc.text.contains(r#"<xs:element name="LookupResponse">"#));
    assert!(rpc.text.contains(r#"xmlns:n0="urn:example:legacy""#));
    assert!(rpc.text.contains(r#"type="n0:Address""#));
    // The encoded binding generates nothing extra.
    assert_eq!(rpc.text.matches("name=\"Lookup\"").count(), 1);

    let root = b.get(ROOT_URI).unwrap();
    assert_eq!(root.text.matches("<xs:import").count(), 1);
    assert!(
        root.text
            .contains(r#"schemaLocation="washboard:/rpc/0.xsd""#)
    );
    assert_eq!(w.report.rpc_wrappers_generated, 2);
}

#[test]
fn structural_report_has_counts_but_no_names() {
    let w = customer();
    let r = &w.report;
    assert_eq!(r.files_supplied, 6);
    assert_eq!(r.files_used, 6);
    assert_eq!(r.wsdl_files, 2);
    assert_eq!(r.xsd_files, 4);
    assert_eq!(r.files_with_bom, 2);
    assert_eq!(r.max_import_depth, 4);
    assert_eq!(r.wsdl_imports, 1);
    assert_eq!(r.xs_imports, 3);
    assert_eq!(r.xs_includes, 1);
    assert_eq!(r.inline_schemas, 1);
    assert_eq!(r.namespaces, 4);
    assert_eq!(r.services, 1);
    assert_eq!(r.ports, 2);
    assert_eq!(r.bindings_soap11, 1);
    assert_eq!(r.bindings_soap12, 1);
    assert_eq!(r.operations, 3);
    assert_eq!(r.operations_supported, 2);
    assert_eq!(r.operations_with_headers, 1);
    assert_eq!(r.operations_without_soap_action, 1);
    assert_eq!(r.abstract_declarations, 3);
    assert_eq!(r.substitution_group_members, 2);
    assert_eq!(r.wildcards, 1);
    assert_eq!(r.bundle_documents, 6);
    assert_eq!(r.errors, 0);
    let text = r.to_string();
    for secret in ["urn:", "Customer", "customer", ".xsd", ".wsdl", "http"] {
        assert!(!text.contains(secret), "report leaks {secret:?}:\n{text}");
    }
}

// ---------------------------------------------------------------------------------------
// Import resolution

#[test]
fn remote_missing_and_ambiguous_references() {
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:b" schemaLocation="https://example.invalid/schemas/v2/b.xsd"/>
      <xs:import namespace="urn:c" schemaLocation="http://example.invalid/c.xsd"/>
      <xs:import namespace="urn:d" schemaLocation="http://example.invalid/d.xsd?xsd=1"/>
      <xs:include schemaLocation="missing.xsd"/>
    </xs:schema>"#;
    let w = mem(&[
        ("/p/svc/S.wsdl", &wsdl(types, "")),
        ("/p/v1/b.xsd", &xsd("urn:b", "")),
        ("/p/v2/b.xsd", &xsd("urn:b", "")),
        ("/p/one/c.xsd", &xsd("urn:c", "")),
        ("/p/two/c.xsd", &xsd("urn:c", "")),
    ]);
    let refs = &w.check.references;
    assert_eq!(refs.len(), 4);
    assert_eq!(
        refs[0].resolution,
        Resolution::Resolved {
            file: "v2/b.xsd".into(),
            matched_by: MatchedBy::UrlSuffix { segments: 2 }
        }
    );
    assert_eq!(
        refs[1].resolution,
        Resolution::Unresolved(Unresolved::Ambiguous {
            candidates: vec!["one/c.xsd".into(), "two/c.xsd".into()]
        })
    );
    assert_eq!(
        refs[2].resolution,
        Resolution::Unresolved(Unresolved::NotSupplied)
    );
    assert_eq!(
        refs[3].resolution,
        Resolution::Unresolved(Unresolved::NotSupplied)
    );
    assert!(w.check.has_errors());
    let errs = errors(&w);
    assert_eq!(errs.len(), 3, "{errs:?}");
    assert!(
        errs.iter()
            .any(|m| m.contains("ambiguous") && m.contains("one/c.xsd"))
    );
    assert!(errs.iter().any(|m| m.contains("never fetched")));
    assert!(errs.iter().all(|m| m.starts_with("svc/S.wsdl: ")));
    assert_eq!(refs[0].pos.line, 5);
    // The matched remote file is in the bundle, with the import rewritten.
    assert!(w.bundle.get("washboard:/wsdl/v2/b.xsd").is_some());
    assert!(w.bundle.get("washboard:/wsdl/v1/b.xsd").is_none());
    let inline = w.bundle.get("washboard:/inline/0.xsd").unwrap();
    assert!(
        inline
            .text
            .contains(r#"schemaLocation="washboard:/wsdl/v2/b.xsd""#)
    );
    assert!(!w.check.files[1].used);
    assert_eq!(w.check.files[1].kind, FileKind::Unused);
    assert_eq!(w.report.refs_matched_by_url, 1);
    assert_eq!(w.report.refs_ambiguous, 1);
    assert_eq!(w.report.refs_unresolved, 2);
}

#[test]
fn relative_reference_falls_back_to_file_name_with_warning() {
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:x" schemaLocation="xsd/X.xsd"/>
      <xs:import namespace="urn:y" schemaLocation="schemas/y.xsd"/>
    </xs:schema>"#;
    let w = mem(&[
        ("/a/S.wsdl", &wsdl(types, "")),
        ("/a/xsd/x.xsd", &xsd("urn:x", "")),
        ("/b/y.xsd", &xsd("urn:y", "")),
    ]);
    assert!(!w.check.has_errors());
    let r = &w.check.references;
    assert_eq!(
        r[0].resolution,
        Resolution::Resolved {
            file: "a/xsd/x.xsd".into(),
            matched_by: MatchedBy::PathIgnoringCase
        }
    );
    assert_eq!(
        r[1].resolution,
        Resolution::Resolved {
            file: "b/y.xsd".into(),
            matched_by: MatchedBy::PathSuffix { segments: 1 }
        }
    );
    let warns = warnings(&w);
    assert!(warns.iter().any(|m| m.contains("ignoring case")));
    assert!(warns.iter().any(|m| m.contains("same file name")));
}

#[test]
fn import_cycles_terminate() {
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:a" schemaLocation="a.xsd"/>
    </xs:schema>"#;
    let a = xsd(
        "urn:a",
        r#"<xs:import namespace="urn:b" schemaLocation="b.xsd"/>
           <xs:include schemaLocation="a2.xsd"/>"#,
    );
    let a2 = xsd("urn:a", r#"<xs:include schemaLocation="a.xsd"/>"#);
    let b = xsd(
        "urn:b",
        r#"<xs:import namespace="urn:a" schemaLocation="a.xsd"/>"#,
    );
    let w = mem(&[
        ("S.wsdl", &wsdl(types, "")),
        ("a.xsd", &a),
        ("a2.xsd", &a2),
        ("b.xsd", &b),
    ]);
    assert!(!w.check.has_errors());
    assert_eq!(w.check.references.len(), 5);
    assert!(w.check.split_namespaces.is_empty());
    assert_closed(&w.bundle);
    let root = w.bundle.get(ROOT_URI).unwrap();
    assert_eq!(root.text.matches("<xs:import").count(), 3);
    assert!(
        root.text
            .contains(r#"schemaLocation="washboard:/wsdl/a.xsd""#)
    );
    assert!(!root.text.contains("a2.xsd"));
}

#[test]
fn split_namespaces_are_detected_and_combined() {
    let types = r#"
    <xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:x" schemaLocation="x1.xsd"/>
    </xs:schema>
    <xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:x" schemaLocation="x2.xsd"/>
    </xs:schema>"#;
    let w = mem(&[
        ("S.wsdl", &wsdl(types, "")),
        ("x1.xsd", &xsd("urn:x", r#"<xs:element name="A"/>"#)),
        ("x2.xsd", &xsd("urn:x", r#"<xs:element name="B"/>"#)),
    ]);
    assert!(!w.check.has_errors());
    let splits = &w.check.split_namespaces;
    assert_eq!(splits.len(), 2);
    assert_eq!(splits[0].namespace, "urn:t");
    assert_eq!(
        splits[0].documents,
        ["S.wsdl (inline schema 1)", "S.wsdl (inline schema 2)"]
    );
    assert_eq!(splits[1].namespace, "urn:x");
    assert_eq!(splits[1].documents, ["x1.xsd", "x2.xsd"]);
    assert_eq!(
        warnings(&w)
            .iter()
            .filter(|m| m.contains("is split across"))
            .count(),
        2
    );
    assert_closed(&w.bundle);
    let x = w.bundle.get(&splits[1].combined_as).unwrap();
    assert_eq!(x.origin, SchemaOrigin::Generated);
    assert_eq!(x.target_ns, "urn:x");
    assert!(
        x.text
            .contains(r#"<xs:include schemaLocation="washboard:/wsdl/x1.xsd"/>"#)
    );
    assert!(
        x.text
            .contains(r#"<xs:include schemaLocation="washboard:/wsdl/x2.xsd"/>"#)
    );
    // Both inline schemas import the combined document, not their original files.
    for i in 0..2 {
        let d = w.bundle.get(&format!("washboard:/inline/{i}.xsd")).unwrap();
        assert!(
            d.text
                .contains(&format!("schemaLocation=\"{}\"", splits[1].combined_as))
        );
    }
    let root = w.bundle.get(ROOT_URI).unwrap();
    assert_eq!(root.text.matches("<xs:import").count(), 2);
}

#[test]
fn import_without_location_points_at_the_namespace_document() {
    let types = r#"
    <xs:schema targetNamespace="urn:a"><xs:element name="A"/></xs:schema>
    <xs:schema targetNamespace="urn:b" xmlns:a="urn:a">
      <xs:import namespace="urn:a"/>
      <xs:import namespace="urn:nowhere"/>
      <xs:element name="B"><xs:complexType><xs:sequence>
        <xs:element ref="a:A"/></xs:sequence></xs:complexType></xs:element>
    </xs:schema>"#;
    let w = mem(&[("S.wsdl", &wsdl(types, ""))]);
    assert!(!w.check.has_errors());
    let r = &w.check.references;
    assert_eq!(r[0].resolution, Resolution::ByNamespace);
    assert_eq!(
        r[1].resolution,
        Resolution::Unresolved(Unresolved::NoLocation)
    );
    assert!(warnings(&w).iter().any(|m| m.contains("urn:nowhere")));
    let b = w.bundle.get("washboard:/inline/1.xsd").unwrap();
    assert!(
        b.text
            .contains(r#"<xs:import schemaLocation="washboard:/inline/0.xsd" namespace="urn:a"/>"#)
    );
    assert!(b.text.contains(r#"<xs:import namespace="urn:nowhere"/>"#));
}

#[test]
fn wsdl_import_of_an_xsd_is_a_schema_import() {
    let entry = format!(
        r#"<wsdl:definitions xmlns:wsdl="{WSDL_NS}" targetNamespace="urn:t">
  <wsdl:import namespace="urn:x" location="types/x.xsd"/>
</wsdl:definitions>"#
    );
    let w = mem(&[("S.wsdl", &entry), ("types/x.xsd", &xsd("urn:x", ""))]);
    assert!(!w.check.has_errors());
    assert_eq!(w.check.references[0].kind, RefKind::WsdlImport);
    assert!(w.bundle.get("washboard:/wsdl/types/x.xsd").is_some());
    let root = w.bundle.get(ROOT_URI).unwrap();
    assert!(root.text.contains(
        r#"<xs:import namespace="urn:x" schemaLocation="washboard:/wsdl/types/x.xsd"/>"#
    ));
}

#[test]
fn xsd11_constructs_are_flagged() {
    let x = format!(
        r#"<xs:schema xmlns:xs="{XSD_NS}" xmlns:vc="http://www.w3.org/2007/XMLSchema-versioning"
  targetNamespace="urn:x" vc:minVersion="1.1">
  <xs:complexType name="T">
    <xs:sequence/>
    <xs:assert test="true()"/>
  </xs:complexType>
</xs:schema>"#
    );
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:x" schemaLocation="x.xsd"/></xs:schema>"#;
    let w = mem(&[("S.wsdl", &wsdl(types, "")), ("x.xsd", &x)]);
    assert!(!w.check.has_errors());
    let found: Vec<&str> = w.check.xsd11.iter().map(|c| c.construct.as_str()).collect();
    assert!(found.contains(&"xs:assert"), "{found:?}");
    assert!(found.contains(&"vc:minVersion"), "{found:?}");
    let assert_pos = w
        .check
        .xsd11
        .iter()
        .find(|c| c.construct == "xs:assert")
        .unwrap();
    assert_eq!(assert_pos.file, "x.xsd");
    assert_eq!(assert_pos.pos.line, 5);
    assert!(warnings(&w).iter().any(|m| m.contains("XSD 1.1")));
}

// ---------------------------------------------------------------------------------------
// Operations

#[test]
fn body_parts_headers_and_per_operation_style() {
    let types = r#"<xs:schema targetNamespace="urn:t" elementFormDefault="qualified">
      <xs:element name="H" type="xs:string"/>
      <xs:element name="HH" type="xs:string"/>
      <xs:element name="A" type="xs:string"/>
      <xs:element name="B" type="xs:string"/>
      <xs:complexType name="Pt"><xs:sequence/></xs:complexType>
    </xs:schema>"#;
    let rest = r#"
  <wsdl:message name="RpcIn">
    <wsdl:part name="p1" type="xs:int"/>
    <wsdl:part name="p2" type="tns:Pt"/>
    <wsdl:part name="h" element="tns:H"/>
  </wsdl:message>
  <wsdl:message name="DocIn">
    <wsdl:part name="a" element="tns:A"/>
    <wsdl:part name="b" element="tns:B"/>
  </wsdl:message>
  <wsdl:message name="Hdr"><wsdl:part name="hh" element="tns:HH"/></wsdl:message>
  <wsdl:message name="Empty"/>
  <wsdl:portType name="PT">
    <wsdl:operation name="R"><wsdl:input message="tns:RpcIn"/><wsdl:output message="tns:Empty"/></wsdl:operation>
    <wsdl:operation name="D"><wsdl:input message="tns:DocIn"/></wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="B" type="tns:PT">
    <soap:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="R">
      <soap:operation soapAction="" style="rpc"/>
      <wsdl:input>
        <soap:body use="literal" namespace="urn:rpc"/>
        <soap:header message="tns:RpcIn" part="h" use="literal"/>
      </wsdl:input>
      <wsdl:output><soap:body use="literal" namespace="urn:rpc"/></wsdl:output>
    </wsdl:operation>
    <wsdl:operation name="D">
      <wsdl:input>
        <soap:body use="literal" parts="b"/>
        <soap:header message="tns:Hdr" part="hh" use="literal"/>
      </wsdl:input>
    </wsdl:operation>
  </wsdl:binding>"#;
    let w = mem(&[("S.wsdl", &wsdl(types, rest))]);
    assert!(!w.check.has_errors());
    let b = w.definitions.binding(&q("urn:t", "B")).unwrap();
    let r = &b.operations[0];
    assert_eq!(r.style, Style::Rpc);
    assert_eq!(r.soap_action, None, "empty soapAction is None");
    let rin = r.input.as_ref().unwrap();
    let names: Vec<&str> = rin.body_parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["p1", "p2"], "header part excluded from body");
    let d = &b.operations[1];
    assert_eq!(d.style, Style::Document);
    let din = d.input.as_ref().unwrap();
    let names: Vec<&str> = din.body_parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["b"], "soap:body parts honoured");
    assert_eq!(din.headers[0].message, q("urn:t", "Hdr"));

    assert!(w.dispatch(&q("urn:t", "A"), None).is_none());
    let dd = w.dispatch(&q("urn:t", "B"), None).unwrap();
    assert_eq!(dd.operation.operation, "D");
    assert_eq!(dd.header_elements, [q("urn:t", "HH")]);
    let rd = w.dispatch(&q("urn:rpc", "R"), None).unwrap();
    assert_eq!(rd.style, Style::Rpc);
    assert_eq!(rd.header_elements, [q("urn:t", "H")]);

    // rpc wrapper in a namespace with no real schema: generated alone, importing urn:t.
    assert_closed(&w.bundle);
    let rpc = w
        .bundle
        .docs
        .iter()
        .find(|d| d.target_ns == "urn:rpc")
        .unwrap();
    assert!(!rpc.text.contains("<xs:include"));
    assert!(
        rpc.text
            .contains(r#"<xs:import namespace="urn:t" schemaLocation="washboard:/inline/0.xsd"/>"#)
    );
    assert!(
        rpc.text
            .contains(r#"<xs:element name="p1" type="xs:int"/>"#)
    );
    assert!(rpc.text.contains(r#"<xs:element name="RResponse">"#));
}

#[test]
fn broken_references_mark_operations_instead_of_failing() {
    let rest = r#"
  <wsdl:portType name="PT">
    <wsdl:operation name="X"><wsdl:input message="tns:Nope"/></wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="B" type="tns:PT">
    <soap:binding style="document"/>
    <wsdl:operation name="X"><wsdl:input><soap:body use="literal"/></wsdl:input></wsdl:operation>
    <wsdl:operation name="Y"><wsdl:input><soap:body use="literal"/></wsdl:input></wsdl:operation>
  </wsdl:binding>
  <wsdl:binding name="Orphan" type="tns:Missing">
    <soap:binding style="document"/>
    <wsdl:operation name="Z"/>
  </wsdl:binding>
  <wsdl:service name="S"><wsdl:port name="P" binding="tns:Gone">
    <soap:address location="http://x.invalid/"/></wsdl:port></wsdl:service>"#;
    let w = mem(&[("S.wsdl", &wsdl("", rest))]);
    assert!(!w.check.has_errors());
    let b = w.definitions.binding(&q("urn:t", "B")).unwrap();
    assert_eq!(b.operations.len(), 2);
    for op in &b.operations {
        assert!(matches!(
            op.support,
            Support::Unsupported(UnsupportedReason::Invalid(_))
        ));
    }
    let orphan = w.definitions.binding(&q("urn:t", "Orphan")).unwrap();
    assert!(!orphan.operations[0].is_supported());
    let warns = warnings(&w);
    assert!(
        warns
            .iter()
            .any(|m| m.contains("tns:Nope") || m.contains("{urn:t}Nope"))
    );
    assert!(warns.iter().any(|m| m.contains("unknown binding")));
    assert_eq!(w.report.operations_invalid, 3);
}

// ---------------------------------------------------------------------------------------
// Inline extraction and encodings

#[test]
fn default_namespace_is_carried_over_and_own_declarations_kept() {
    let entry = format!(
        r#"<definitions xmlns="{WSDL_NS}" xmlns:xs="{XSD_NS}" xmlns:tns="urn:outer" targetNamespace="urn:t">
<types>
<xs:schema xmlns:tns="urn:inner" targetNamespace="urn:inner"><xs:element name="E" type="tns:T"/>
<xs:complexType name="T"/></xs:schema>
</types>
</definitions>"#
    );
    let w = mem(&[("S.wsdl", &entry)]);
    assert!(!w.check.has_errors());
    let d = w.bundle.get("washboard:/inline/0.xsd").unwrap();
    assert!(d.text.contains(&format!(r#"xmlns="{WSDL_NS}""#)));
    assert_eq!(d.text.matches("xmlns:tns=").count(), 1);
    assert!(d.text.contains(r#"xmlns:tns="urn:inner""#));
    assert!(d.text.starts_with("\n\n<xs:schema"), "schema is on line 3");
}

#[test]
fn encodings_are_decoded_and_declarations_fixed() {
    let x = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:x\"><!-- ü --></xs:schema>";
    let mut x16 = vec![0xFF, 0xFE];
    for u in x.encode_utf16() {
        x16.extend_from_slice(&u.to_le_bytes());
    }
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:x" schemaLocation="x.xsd"/></xs:schema>"#;
    let latin1: Vec<u8> = wsdl(types, "<!-- \u{fc} -->")
        .replace("encoding=\"UTF-8\"", "encoding=\"ISO-8859-1\"")
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap())
        .collect();
    let w = load(&Sources::new(
        SourceFile::new("S.wsdl", latin1),
        [SourceFile::new("x.xsd", x16)],
    ));
    print_diags(&w);
    assert!(!w.check.has_errors());
    assert_eq!(
        w.check.files[0].encoding,
        Some(crate::xml::Encoding::Latin1)
    );
    assert_eq!(
        w.check.files[1].encoding,
        Some(crate::xml::Encoding::Utf16Le)
    );
    assert!(w.check.files[1].had_bom);
    let d = w.bundle.get("washboard:/wsdl/x.xsd").unwrap();
    assert!(
        d.text
            .starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>")
    );
    assert!(d.text.contains("ü"));
    assert_eq!(w.report.files_utf16, 1);
    assert_eq!(w.report.files_latin1, 1);
}

#[test]
fn bad_entries_are_reported_not_panicking() {
    let w = mem(&[("S.wsdl", "<wsdl:definitions")]);
    assert!(w.check.has_errors());
    assert!(errors(&w)[0].contains("not well-formed"));
    assert_eq!(w.check.files[0].kind, FileKind::Unreadable);

    let w = mem(&[("S.wsdl", &xsd("urn:x", ""))]);
    assert!(errors(&w).iter().any(|m| m.contains("not a WSDL 1.1")));

    let w = mem(&[(
        "S.wsdl",
        r#"<description xmlns="http://www.w3.org/ns/wsdl"/>"#,
    )]);
    assert!(errors(&w).iter().any(|m| m.contains("WSDL 2.0")));

    let w = load(&Sources::new(
        SourceFile::new("S.wsdl", vec![0xEF, 0xBB, 0xBF, 0xFF, 0xFE]),
        [],
    ));
    assert!(w.check.has_errors());

    // A referenced file that is not XML at all.
    let types = r#"<xs:schema targetNamespace="urn:t">
      <xs:include schemaLocation="x.xsd"/></xs:schema>"#;
    let w = mem(&[("S.wsdl", &wsdl(types, "")), ("x.xsd", "garbage")]);
    assert!(
        errors(&w)
            .iter()
            .any(|m| m.starts_with("x.xsd: not well-formed"))
    );
}

#[test]
fn truncated_inputs_never_panic() {
    let dir = fixtures().join("customer");
    let binding = std::fs::read(dir.join("CustomerBinding.wsdl")).unwrap();
    let service = std::fs::read(dir.join("CustomerService.wsdl")).unwrap();
    let legacy = std::fs::read(fixtures().join("legacy-rpc/Legacy.wsdl")).unwrap();
    for cut in (0..binding.len()).step_by(41) {
        let w = load(&Sources::new(
            SourceFile::new("CustomerService.wsdl", service.clone()),
            [SourceFile::new(
                "CustomerBinding.wsdl",
                binding[..cut].to_vec(),
            )],
        ));
        let _ = w.report.to_string();
    }
    for cut in (0..legacy.len()).step_by(29) {
        let _ = load(&Sources::new(
            SourceFile::new("Legacy.wsdl", legacy[..cut].to_vec()),
            [],
        ));
    }
}

// ---------------------------------------------------------------------------------------
// Sources

#[test]
fn sources_normalize_and_dedupe() {
    let s = Sources::new(
        SourceFile::new("wsdl/./S.wsdl", "a"),
        [
            SourceFile::new("wsdl/../xsd/a.xsd", "b"),
            SourceFile::new("xsd/a.xsd", "dup"),
        ],
    );
    let paths: Vec<&str> = s.files().iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["wsdl/S.wsdl", "xsd/a.xsd"]);
    assert_eq!(s.entry().path, "wsdl/S.wsdl");
}

#[test]
fn from_disk_collects_xsd_and_wsdl_skipping_hidden() {
    let root = std::env::temp_dir().join(format!("washboard-wsdl-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("x/sub")).unwrap();
    std::fs::create_dir_all(root.join("x/.previous")).unwrap();
    std::fs::write(root.join("S.wsdl"), wsdl("", "")).unwrap();
    std::fs::write(root.join("x/a.XSD"), "").unwrap();
    std::fs::write(root.join("x/sub/b.wsdl"), "").unwrap();
    std::fs::write(root.join("x/notes.txt"), "").unwrap();
    std::fs::write(root.join("x/.hidden.xsd"), "").unwrap();
    std::fs::write(root.join("x/.previous/old.xsd"), "").unwrap();
    let s = Sources::from_disk(&root.join("S.wsdl"), &[root.join("x")]).unwrap();
    let names: Vec<String> = s
        .files()
        .iter()
        .map(|f| f.path.rsplit('/').next().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["S.wsdl", "a.XSD", "b.wsdl"]);
    let w = load(&s);
    let dests: Vec<&str> = w.layout.iter().map(|l| l.dest.as_str()).collect();
    assert_eq!(dests, ["S.wsdl", "x/a.XSD", "x/sub/b.wsdl"]);
    assert!(Sources::from_disk(&root.join("missing.wsdl"), &[]).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn file_uris_are_percent_encoded() {
    assert_eq!(
        file_uri("my xsd/ä.xsd"),
        "washboard:/wsdl/my%20xsd/%C3%A4.xsd"
    );
}
