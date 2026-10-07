//! Structural report for `washboard inspect` (PLAN §5.1): counts and flags only, never
//! names, namespaces or contents, so it can be shared from machines holding confidential
//! WSDLs to check them against the plan's assumptions.

use std::collections::HashSet;
use std::fmt;

use crate::diag::Severity;
use crate::model::SchemaBundle;
use crate::xml::Encoding;

use super::defs::{Definitions, Protocol, Style, Support, UnsupportedReason};
use super::graph::{FileKind, RefKind, Walk};
use super::source::{MatchedBy, SourceFile};
use super::{ImportCheck, Resolution, Unresolved};

/// Serializes with the field names as keys (`washboard inspect --json`); renaming a field
/// changes that output.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct StructuralReport {
    pub files_supplied: usize,
    pub files_used: usize,
    pub wsdl_files: usize,
    pub xsd_files: usize,
    pub unreadable_files: usize,
    pub used_bytes: usize,
    pub largest_file_bytes: usize,
    pub files_with_bom: usize,
    pub files_utf16: usize,
    pub files_latin1: usize,
    /// Longest shortest-path from the entry WSDL over all references.
    pub max_import_depth: u32,

    pub wsdl_imports: usize,
    pub xs_imports: usize,
    pub xs_includes: usize,
    pub xs_redefines: usize,
    pub xs_overrides: usize,
    pub imports_without_location: usize,
    pub refs_matched_by_url: usize,
    pub refs_matched_by_name_only: usize,
    pub refs_matched_ignoring_case: usize,
    pub refs_unresolved: usize,
    pub refs_ambiguous: usize,

    pub inline_schemas: usize,
    pub namespaces: usize,
    pub split_namespaces: usize,
    pub xsd11_constructs: usize,
    pub global_elements: usize,
    pub global_complex_types: usize,
    pub global_simple_types: usize,
    pub abstract_declarations: usize,
    pub substitution_group_members: usize,
    pub wildcards: usize,

    pub services: usize,
    pub ports: usize,
    pub bindings_soap11: usize,
    pub bindings_soap12: usize,
    pub bindings_other: usize,
    pub operations: usize,
    pub operations_supported: usize,
    pub operations_document: usize,
    pub operations_rpc: usize,
    pub operations_encoded: usize,
    pub operations_invalid: usize,
    pub operations_with_headers: usize,
    pub operations_without_soap_action: usize,
    pub operations_multi_part_body: usize,
    pub rpc_wrappers_generated: usize,

    pub bundle_documents: usize,
    pub bundle_bytes: usize,
    pub errors: usize,
    pub warnings: usize,
}

pub(crate) fn build(
    files: &[SourceFile],
    walk: &Walk,
    defs: &Definitions,
    check: &ImportCheck,
    bundle: &SchemaBundle,
    rpc_wrappers: usize,
) -> StructuralReport {
    let mut r = StructuralReport {
        files_supplied: files.len(),
        ..StructuralReport::default()
    };
    for (f, st) in walk.files.iter().enumerate() {
        if !st.reached {
            continue;
        }
        r.files_used += 1;
        let len = files.get(f).map_or(0, |s| s.bytes.len());
        r.used_bytes += len;
        r.largest_file_bytes = r.largest_file_bytes.max(len);
        r.max_import_depth = r.max_import_depth.max(st.depth);
        match st.kind {
            FileKind::Wsdl => r.wsdl_files += 1,
            FileKind::Xsd => r.xsd_files += 1,
            FileKind::Unreadable => r.unreadable_files += 1,
            _ => {}
        }
        r.files_with_bom += usize::from(st.had_bom);
        match st.encoding {
            Some(Encoding::Utf16Le | Encoding::Utf16Be) => r.files_utf16 += 1,
            Some(Encoding::Latin1) => r.files_latin1 += 1,
            _ => {}
        }
    }
    for rf in &check.references {
        match rf.kind {
            RefKind::WsdlImport => r.wsdl_imports += 1,
            RefKind::XsdImport => r.xs_imports += 1,
            RefKind::XsdInclude => r.xs_includes += 1,
            RefKind::XsdRedefine => r.xs_redefines += 1,
            RefKind::XsdOverride => r.xs_overrides += 1,
        }
        match &rf.resolution {
            Resolution::Resolved { matched_by, .. } => match matched_by {
                MatchedBy::UrlSuffix { .. } => r.refs_matched_by_url += 1,
                MatchedBy::PathSuffix { .. } => r.refs_matched_by_name_only += 1,
                MatchedBy::PathIgnoringCase => r.refs_matched_ignoring_case += 1,
                MatchedBy::Path => {}
            },
            Resolution::ByNamespace => r.imports_without_location += 1,
            Resolution::Unresolved(Unresolved::Ambiguous { .. }) => r.refs_ambiguous += 1,
            Resolution::Unresolved(Unresolved::NoLocation) if rf.kind == RefKind::XsdImport => {
                r.imports_without_location += 1;
                r.refs_unresolved += 1;
            }
            Resolution::Unresolved(_) => r.refs_unresolved += 1,
        }
    }
    r.inline_schemas = walk.inline.len();
    let mut namespaces: HashSet<&str> = walk.inline.iter().map(|s| s.tns.as_str()).collect();
    for &f in &walk.xsd_order {
        namespaces.insert(walk.files[f].tns.as_str());
    }
    r.namespaces = namespaces.len();
    r.split_namespaces = check.split_namespaces.len();
    r.xsd11_constructs = check.xsd11.len();
    let s = &walk.stats;
    r.global_elements = s.global_elements;
    r.global_complex_types = s.global_complex_types;
    r.global_simple_types = s.global_simple_types;
    r.abstract_declarations = s.abstract_declarations;
    r.substitution_group_members = s.substitution_group_members;
    r.wildcards = s.wildcards;

    r.services = defs.services.len();
    r.ports = defs.services.iter().map(|s| s.ports.len()).sum();
    for b in &defs.bindings {
        match b.protocol {
            Protocol::Soap11 => r.bindings_soap11 += 1,
            Protocol::Soap12 => r.bindings_soap12 += 1,
            Protocol::Other { .. } => r.bindings_other += 1,
        }
        for op in &b.operations {
            r.operations += 1;
            match &op.support {
                Support::Supported => r.operations_supported += 1,
                Support::Unsupported(UnsupportedReason::Encoded) => r.operations_encoded += 1,
                Support::Unsupported(UnsupportedReason::Invalid(_)) => {
                    r.operations_invalid += 1;
                }
                Support::Unsupported(_) => {}
            }
            match op.style {
                Style::Document => r.operations_document += 1,
                Style::Rpc => r.operations_rpc += 1,
            }
            if let Some(m) = &op.input {
                r.operations_with_headers += usize::from(!m.headers.is_empty());
                r.operations_multi_part_body += usize::from(m.body_parts.len() > 1);
            }
            r.operations_without_soap_action += usize::from(op.soap_action.is_none());
        }
    }
    r.rpc_wrappers_generated = rpc_wrappers;
    r.bundle_documents = bundle.docs.len();
    r.bundle_bytes = bundle.docs.iter().map(|d| d.text.len()).sum();
    for d in &check.diagnostics {
        match d.severity {
            Severity::Error => r.errors += 1,
            Severity::Warning => r.warnings += 1,
        }
    }
    r
}

impl fmt::Display for StructuralReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows: &[(&str, String)] = &[
            ("files supplied", self.files_supplied.to_string()),
            ("files used", self.files_used.to_string()),
            ("  wsdl", self.wsdl_files.to_string()),
            ("  xsd", self.xsd_files.to_string()),
            ("  unreadable", self.unreadable_files.to_string()),
            ("bytes used", self.used_bytes.to_string()),
            ("largest file bytes", self.largest_file_bytes.to_string()),
            ("files with BOM", self.files_with_bom.to_string()),
            ("files UTF-16", self.files_utf16.to_string()),
            ("files ISO-8859-1", self.files_latin1.to_string()),
            ("max import depth", self.max_import_depth.to_string()),
            ("wsdl:import", self.wsdl_imports.to_string()),
            ("xs:import", self.xs_imports.to_string()),
            ("xs:include", self.xs_includes.to_string()),
            ("xs:redefine", self.xs_redefines.to_string()),
            ("xs:override", self.xs_overrides.to_string()),
            (
                "imports without location",
                self.imports_without_location.to_string(),
            ),
            (
                "refs matched by URL suffix",
                self.refs_matched_by_url.to_string(),
            ),
            (
                "refs matched by name only",
                self.refs_matched_by_name_only.to_string(),
            ),
            (
                "refs matched ignoring case",
                self.refs_matched_ignoring_case.to_string(),
            ),
            ("refs unresolved", self.refs_unresolved.to_string()),
            ("refs ambiguous", self.refs_ambiguous.to_string()),
            ("inline schemas", self.inline_schemas.to_string()),
            ("namespaces", self.namespaces.to_string()),
            ("split namespaces", self.split_namespaces.to_string()),
            ("XSD 1.1 constructs", self.xsd11_constructs.to_string()),
            ("global elements", self.global_elements.to_string()),
            (
                "global complex types",
                self.global_complex_types.to_string(),
            ),
            ("global simple types", self.global_simple_types.to_string()),
            (
                "abstract declarations",
                self.abstract_declarations.to_string(),
            ),
            (
                "substitution group members",
                self.substitution_group_members.to_string(),
            ),
            ("wildcards", self.wildcards.to_string()),
            ("services", self.services.to_string()),
            ("ports", self.ports.to_string()),
            ("bindings SOAP 1.1", self.bindings_soap11.to_string()),
            ("bindings SOAP 1.2", self.bindings_soap12.to_string()),
            ("bindings other", self.bindings_other.to_string()),
            ("operations", self.operations.to_string()),
            ("  supported", self.operations_supported.to_string()),
            ("  document", self.operations_document.to_string()),
            ("  rpc", self.operations_rpc.to_string()),
            ("  encoded", self.operations_encoded.to_string()),
            ("  inconsistent", self.operations_invalid.to_string()),
            ("  with headers", self.operations_with_headers.to_string()),
            (
                "  without soapAction",
                self.operations_without_soap_action.to_string(),
            ),
            (
                "  multi-part body",
                self.operations_multi_part_body.to_string(),
            ),
            (
                "rpc wrappers generated",
                self.rpc_wrappers_generated.to_string(),
            ),
            ("bundle documents", self.bundle_documents.to_string()),
            ("bundle bytes", self.bundle_bytes.to_string()),
            ("errors", self.errors.to_string()),
            ("warnings", self.warnings.to_string()),
        ];
        for (k, v) in rows {
            writeln!(f, "{k:<28} {v}")?;
        }
        Ok(())
    }
}
