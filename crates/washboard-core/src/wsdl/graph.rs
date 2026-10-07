//! The import walk: starting at the entry WSDL, follow `wsdl:import`, `xs:import`,
//! `xs:include`, `xs:redefine` (and the XSD 1.1 `xs:override`, which is flagged) through the
//! supplied files. Breadth-first, each file visited once, so cycles terminate and the
//! recorded depth is the shortest import chain.

use std::collections::{HashSet, VecDeque};
use std::ops::Range;

use crate::diag::{DiagSource, Diagnostic, TextPos};
use crate::soap::{WSDL_NS, XSD_NS};
use crate::xml::{self, Encoding};

use super::source::{FileIndex, Location, Match, MatchedBy, SourceFile, classify};
use super::text::{LineIndex, start_tag};

/// WSDL 2.0 namespace; such files are recognized only to give a clear error.
pub(crate) const WSDL20_NS: &str = "http://www.w3.org/ns/wsdl";
/// XSD 1.1 versioning namespace (`vc:minVersion` …).
pub(crate) const VC_NS: &str = "http://www.w3.org/2007/XMLSchema-versioning";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Parses decoded XML text. Internal DTD subsets occur in old WSDLs, so they are allowed;
/// roxmltree never loads external entities and guards against entity expansion bombs.
pub(crate) fn parse(text: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text, opts)
}

/// The kind of reference element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    WsdlImport,
    XsdImport,
    XsdInclude,
    XsdRedefine,
    /// XSD 1.1; reported as unsupported but still followed so the check is complete.
    XsdOverride,
}

impl RefKind {
    pub fn label(self) -> &'static str {
        match self {
            RefKind::WsdlImport => "wsdl:import",
            RefKind::XsdImport => "xs:import",
            RefKind::XsdInclude => "xs:include",
            RefKind::XsdRedefine => "xs:redefine",
            RefKind::XsdOverride => "xs:override",
        }
    }
}

/// Why a reference could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// No supplied file matches the location.
    NotSupplied,
    /// Several supplied files match equally well; the user must remove the wrong ones.
    Ambiguous { candidates: Vec<String> },
    /// No location given, and (for `xs:import`) no supplied schema defines the namespace.
    NoLocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// `file` is the project-relative destination (see [`super::LayoutEntry::dest`]).
    Resolved {
        file: String,
        matched_by: MatchedBy,
    },
    /// `xs:import` without `schemaLocation` whose namespace a supplied schema defines.
    ByNamespace,
    Unresolved(Unresolved),
}

/// One reference as shown in the New Project sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: RefKind,
    /// Project-relative path of the file containing the reference.
    pub from: String,
    /// For references inside `wsdl:types`: index of the `xs:schema` in that WSDL.
    pub inline_schema: Option<usize>,
    pub pos: TextPos,
    pub location: Option<String>,
    pub namespace: Option<String>,
    pub resolution: Resolution,
}

/// An XSD 1.1 construct; libxml2 implements XSD 1.0 only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xsd11Construct {
    pub file: String,
    pub pos: TextPos,
    /// E.g. `xs:assert`, `vc:minVersion`.
    pub construct: String,
}

/// What a supplied file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Wsdl,
    Xsd,
    /// WSDL 2.0 is out of scope.
    Wsdl20,
    /// Well-formed XML that is neither.
    OtherXml,
    /// Could not be decoded or is not well-formed.
    Unreadable,
    /// Not referenced, so not read.
    Unused,
}

/// Who contains a reference: a file's root schema/definitions, or an inline schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    File(usize),
    Inline(usize),
}

#[derive(Debug)]
pub(crate) struct FileState {
    pub reached: bool,
    pub depth: u32,
    pub text: Option<String>,
    pub kind: FileKind,
    pub tns: String,
    pub encoding: Option<Encoding>,
    pub had_bom: bool,
    /// Reached through the `wsdl:import` closure (contributes definitions).
    pub in_wsdl_closure: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InlineSchema {
    pub file: usize,
    /// Index among the `xs:schema` elements of the file's `wsdl:types`.
    pub index: usize,
    pub tns: String,
    /// Byte range of the `xs:schema` element in the file's decoded text.
    pub range: Range<usize>,
}

/// Internal data for a [`Reference`], parallel to `Walk::refs`.
#[derive(Debug, Clone)]
pub(crate) struct RefSite {
    pub owner: Owner,
    pub target: Option<usize>,
    /// Range of the location attribute's value, if present.
    pub loc_value: Option<Range<usize>>,
    /// Byte offset just after the element name, for inserting a missing `schemaLocation`.
    pub name_end: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SchemaStats {
    pub global_elements: usize,
    pub global_complex_types: usize,
    pub global_simple_types: usize,
    pub abstract_declarations: usize,
    pub substitution_group_members: usize,
    pub wildcards: usize,
}

#[derive(Debug)]
pub(crate) struct Walk {
    pub files: Vec<FileState>,
    pub inline: Vec<InlineSchema>,
    pub refs: Vec<Reference>,
    pub sites: Vec<RefSite>,
    /// WSDL files of the `wsdl:import` closure, entry first, breadth-first.
    pub wsdl_order: Vec<usize>,
    /// XSD files reached, breadth-first.
    pub xsd_order: Vec<usize>,
    pub diags: Vec<Diagnostic>,
    pub xsd11: Vec<Xsd11Construct>,
    pub stats: SchemaStats,
}

impl Walk {
    pub(crate) fn owner_file(&self, o: Owner) -> usize {
        match o {
            Owner::File(f) => f,
            Owner::Inline(i) => self.inline[i].file,
        }
    }
}

/// Runs the walk. `names` are the display (project-relative) paths, parallel to `files`.
pub(crate) fn walk(files: &[SourceFile], names: &[String]) -> Walk {
    let index = FileIndex::new(files);
    let mut w = Walk {
        files: files
            .iter()
            .map(|_| FileState {
                reached: false,
                depth: 0,
                text: None,
                kind: FileKind::Unused,
                tns: String::new(),
                encoding: None,
                had_bom: false,
                in_wsdl_closure: false,
            })
            .collect(),
        inline: Vec::new(),
        refs: Vec::new(),
        sites: Vec::new(),
        wsdl_order: Vec::new(),
        xsd_order: Vec::new(),
        diags: Vec::new(),
        xsd11: Vec::new(),
        stats: SchemaStats::default(),
    };
    let mut queue: VecDeque<(usize, bool)> = VecDeque::new();
    if files.is_empty() {
        return w;
    }
    w.files[0].reached = true;
    queue.push_back((0, true));
    while let Some((f, via_wsdl)) = queue.pop_front() {
        let fresh = w.files[f].text.is_none() && w.files[f].kind == FileKind::Unused;
        if fresh {
            load(&mut w, f, &files[f], &names[f]);
            if w.files[f].kind == FileKind::Xsd {
                w.xsd_order.push(f);
            }
        }
        if via_wsdl && w.files[f].kind == FileKind::Wsdl && !w.files[f].in_wsdl_closure {
            w.files[f].in_wsdl_closure = true;
            w.wsdl_order.push(f);
        }
        if !fresh {
            // Already scanned when first reached through another kind of reference.
            continue;
        }
        if f == 0 {
            match w.files[0].kind {
                FileKind::Wsdl => {}
                FileKind::Wsdl20 => w.diags.push(err(
                    None,
                    format!("{}: WSDL 2.0 is not supported (WSDL 1.1 only)", names[0]),
                )),
                FileKind::Unreadable => {}
                _ => w.diags.push(err(
                    None,
                    format!("{}: the entry file is not a WSDL 1.1 document", names[0]),
                )),
            }
        }
        let first_refs = w.refs.len();
        scan(&mut w, f, &names[f]);
        // Resolve and enqueue the references found in this file.
        let depth = w.files[f].depth + 1;
        for r in first_refs..w.refs.len() {
            resolve(&mut w, r, files, names, &index);
            if let Some(t) = w.sites[r].target {
                let via = w.refs[r].kind == RefKind::WsdlImport;
                if !w.files[t].reached {
                    w.files[t].reached = true;
                    w.files[t].depth = depth;
                    queue.push_back((t, via));
                } else if via && !w.files[t].in_wsdl_closure {
                    queue.push_back((t, via));
                }
            }
        }
    }
    check_targets(&mut w, names);
    resolve_by_namespace(&mut w);
    w
}

fn err(pos: Option<TextPos>, msg: String) -> Diagnostic {
    Diagnostic::error(DiagSource::Import, pos, msg)
}

fn warn(pos: Option<TextPos>, msg: String) -> Diagnostic {
    Diagnostic::warning(DiagSource::Import, pos, msg)
}

fn load(w: &mut Walk, f: usize, file: &SourceFile, name: &str) {
    let st = &mut w.files[f];
    let decoded = match xml::decode(&file.bytes) {
        Ok(d) => d,
        Err(e) => {
            st.kind = FileKind::Unreadable;
            w.diags.push(err(None, format!("{name}: {e}")));
            return;
        }
    };
    st.encoding = Some(decoded.encoding);
    st.had_bom = decoded.had_bom;
    let text = decoded.text;
    match parse(&text) {
        Ok(doc) => {
            let root = doc.root_element();
            let ns = root.tag_name().namespace().unwrap_or("");
            st.kind = match (ns, root.tag_name().name()) {
                (WSDL_NS, "definitions") => FileKind::Wsdl,
                (XSD_NS, "schema") => FileKind::Xsd,
                (WSDL20_NS, "description") => FileKind::Wsdl20,
                _ => FileKind::OtherXml,
            };
            st.tns = root
                .attribute("targetNamespace")
                .unwrap_or_default()
                .to_owned();
        }
        Err(e) => {
            st.kind = FileKind::Unreadable;
            let p = e.pos();
            w.diags.push(err(
                Some(TextPos {
                    line: p.row,
                    column: p.col,
                }),
                format!("{name}: not well-formed XML: {e}"),
            ));
        }
    }
    st.text = Some(text);
}

/// Extracts references (and inline schemas, XSD 1.1 constructs, stats) from a loaded file.
fn scan(w: &mut Walk, f: usize, name: &str) {
    let Some(text) = w.files[f].text.take() else {
        return;
    };
    if let Ok(doc) = parse(&text) {
        let lines = LineIndex::new(&text);
        let root = doc.root_element();
        match w.files[f].kind {
            FileKind::Wsdl => {
                let mut index = 0;
                for child in root.children().filter(|n| n.is_element()) {
                    if child.tag_name().namespace() != Some(WSDL_NS) {
                        continue;
                    }
                    match child.tag_name().name() {
                        "import" => add_ref(
                            w,
                            &text,
                            &lines,
                            child,
                            RefKind::WsdlImport,
                            "location",
                            Owner::File(f),
                            name,
                        ),
                        "types" => {
                            for s in child.children().filter(|n| is_xs(n, "schema")) {
                                let id = w.inline.len();
                                w.inline.push(InlineSchema {
                                    file: f,
                                    index,
                                    tns: s.attribute("targetNamespace").unwrap_or("").to_owned(),
                                    range: s.range(),
                                });
                                index += 1;
                                scan_schema(w, &text, &lines, s, Owner::Inline(id), name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            FileKind::Xsd => scan_schema(w, &text, &lines, root, Owner::File(f), name),
            _ => {}
        }
    }
    w.files[f].text = Some(text);
}

fn is_xs(n: &roxmltree::Node<'_, '_>, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(XSD_NS) && n.tag_name().name() == local
}

fn scan_schema(
    w: &mut Walk,
    text: &str,
    lines: &LineIndex<'_>,
    schema: roxmltree::Node<'_, '_>,
    owner: Owner,
    name: &str,
) {
    for child in schema.children().filter(|n| n.is_element()) {
        if child.tag_name().namespace() != Some(XSD_NS) {
            continue;
        }
        let kind = match child.tag_name().name() {
            "import" => RefKind::XsdImport,
            "include" => RefKind::XsdInclude,
            "redefine" => RefKind::XsdRedefine,
            "override" => RefKind::XsdOverride,
            "element" => {
                w.stats.global_elements += 1;
                continue;
            }
            "complexType" => {
                w.stats.global_complex_types += 1;
                continue;
            }
            "simpleType" => {
                w.stats.global_simple_types += 1;
                continue;
            }
            _ => continue,
        };
        add_ref(w, text, lines, child, kind, "schemaLocation", owner, name);
    }
    if schema.namespaces().any(|ns| ns.uri() == VC_NS) {
        w.xsd11.push(Xsd11Construct {
            file: name.to_owned(),
            pos: lines.pos(schema.range().start),
            construct: "vc: (XSD 1.1 versioning) namespace".to_owned(),
        });
    }
    for n in schema.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name();
        if tag.namespace() == Some(XSD_NS) {
            match tag.name() {
                "assert" | "assertion" | "alternative" | "override" | "openContent"
                | "defaultOpenContent" => w.xsd11.push(Xsd11Construct {
                    file: name.to_owned(),
                    pos: lines.pos(n.range().start),
                    construct: format!("xs:{}", tag.name()),
                }),
                "any" | "anyAttribute" => w.stats.wildcards += 1,
                _ => {}
            }
            if n.attribute("abstract") == Some("true") {
                w.stats.abstract_declarations += 1;
            }
            if n.has_attribute("substitutionGroup") {
                w.stats.substitution_group_members += 1;
            }
        }
        for a in n.attributes() {
            if a.namespace() == Some(VC_NS) {
                w.xsd11.push(Xsd11Construct {
                    file: name.to_owned(),
                    pos: lines.pos(n.range().start),
                    construct: format!("vc:{}", a.name()),
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_ref(
    w: &mut Walk,
    text: &str,
    lines: &LineIndex<'_>,
    el: roxmltree::Node<'_, '_>,
    kind: RefKind,
    loc_attr: &str,
    owner: Owner,
    name: &str,
) {
    let start = el.range().start;
    let name_end = start_tag(text, start).map_or(start, |t| t.name_end);
    let loc = el.attribute_node(loc_attr);
    w.refs.push(Reference {
        kind,
        from: name.to_owned(),
        inline_schema: match owner {
            Owner::Inline(i) => Some(w.inline[i].index),
            Owner::File(_) => None,
        },
        pos: lines.pos(start),
        location: loc.map(|a| a.value().to_owned()),
        namespace: el.attribute("namespace").map(str::to_owned),
        resolution: Resolution::Unresolved(Unresolved::NoLocation),
    });
    w.sites.push(RefSite {
        owner,
        target: None,
        loc_value: loc.map(|a| a.range_value()),
        name_end,
    });
}

fn resolve(w: &mut Walk, r: usize, files: &[SourceFile], names: &[String], index: &FileIndex) {
    let importer = w.owner_file(w.sites[r].owner);
    let rf = &w.refs[r];
    let what = match &rf.namespace {
        Some(ns) => format!("{} of namespace {ns:?}", rf.kind.label()),
        None => rf.kind.label().to_owned(),
    };
    let loc = rf
        .location
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let Some(loc) = loc else {
        if rf.kind == RefKind::XsdImport {
            // Decided after the walk, when all target namespaces are known.
            w.refs[r].resolution = Resolution::ByNamespace;
        } else {
            let attr = if rf.kind == RefKind::WsdlImport {
                "location"
            } else {
                "schemaLocation"
            };
            w.diags.push(err(
                Some(rf.pos),
                format!("{}: {what} has no {attr}", rf.from),
            ));
        }
        return;
    };
    let classified = classify(loc);
    match index.resolve(&files[importer].path, &classified) {
        Match::Found(t, by) => {
            match by {
                MatchedBy::PathIgnoringCase => w.diags.push(warn(
                    Some(rf.pos),
                    format!(
                        "{}: {what} {loc:?} matches {} only when ignoring case",
                        rf.from, names[t]
                    ),
                )),
                MatchedBy::PathSuffix { .. } => w.diags.push(warn(
                    Some(rf.pos),
                    format!(
                        "{}: {what} {loc:?} is not at the referenced path; using {} \
                         (same file name)",
                        rf.from, names[t]
                    ),
                )),
                MatchedBy::Path | MatchedBy::UrlSuffix { .. } => {}
            }
            w.sites[r].target = Some(t);
            w.refs[r].resolution = Resolution::Resolved {
                file: names[t].clone(),
                matched_by: by,
            };
        }
        Match::Ambiguous(c) => {
            let candidates: Vec<String> = c.iter().map(|&i| names[i].clone()).collect();
            w.diags.push(err(
                Some(rf.pos),
                format!(
                    "{}: {what} {loc:?} is ambiguous; it matches {}. Remove the wrong files.",
                    rf.from,
                    candidates.join(", ")
                ),
            ));
            w.refs[r].resolution = Resolution::Unresolved(Unresolved::Ambiguous { candidates });
        }
        Match::NotFound => {
            let hint = match classified {
                Location::Remote {
                    has_query: true, ..
                } => {
                    " (remote locations are never fetched; save the document and add it, \
                     named like the last path segment of the URL)"
                }
                Location::Remote { .. } => " (remote locations are never fetched; add the file)",
                Location::Path(_) => "",
            };
            w.diags.push(err(
                Some(rf.pos),
                format!("{}: {what} {loc:?} was not supplied{hint}", rf.from),
            ));
            w.refs[r].resolution = Resolution::Unresolved(Unresolved::NotSupplied);
        }
    }
}

/// Checks that each resolved target is the kind of document its reference expects.
fn check_targets(w: &mut Walk, names: &[String]) {
    for r in 0..w.refs.len() {
        let Some(t) = w.sites[r].target else { continue };
        let rf = &w.refs[r];
        let kind = w.files[t].kind;
        let ok = match (rf.kind, kind) {
            (_, FileKind::Unreadable) => true, // already reported
            (RefKind::WsdlImport, FileKind::Wsdl | FileKind::Xsd) => true,
            (_, FileKind::Xsd | FileKind::Wsdl) => true,
            _ => false,
        };
        if !ok {
            let d = err(
                Some(rf.pos),
                format!(
                    "{}: {} target {} is not a WSDL 1.1 or XSD document",
                    rf.from,
                    rf.kind.label(),
                    names[t]
                ),
            );
            w.diags.push(d);
            continue;
        }
        if let Some(ns) = &rf.namespace
            && matches!(rf.kind, RefKind::XsdImport | RefKind::WsdlImport)
            && matches!(kind, FileKind::Xsd | FileKind::Wsdl)
            && *ns != w.files[t].tns
            && !(kind == FileKind::Wsdl && rf.kind == RefKind::XsdImport)
        {
            let d = warn(
                Some(rf.pos),
                format!(
                    "{}: {} declares namespace {ns:?} but {} has targetNamespace {:?}",
                    rf.from,
                    rf.kind.label(),
                    names[t],
                    w.files[t].tns
                ),
            );
            w.diags.push(d);
        }
    }
}

/// Settles `xs:import`s without `schemaLocation` now that all schemas are known.
fn resolve_by_namespace(w: &mut Walk) {
    let mut known: HashSet<&str> = w.inline.iter().map(|s| s.tns.as_str()).collect();
    for &f in &w.xsd_order {
        known.insert(w.files[f].tns.as_str());
    }
    known.insert(XSD_NS);
    let mut missing = Vec::new();
    for (r, rf) in w.refs.iter().enumerate() {
        if rf.resolution == Resolution::ByNamespace {
            let ns = rf.namespace.as_deref().unwrap_or("");
            if !known.contains(ns) {
                missing.push(r);
            }
        }
    }
    for r in missing {
        let rf = &mut w.refs[r];
        rf.resolution = Resolution::Unresolved(Unresolved::NoLocation);
        let ns = rf.namespace.as_deref().unwrap_or("");
        let hint = if ns == XML_NS {
            " (add the W3C xml.xsd file)"
        } else {
            ""
        };
        let d = warn(
            Some(rf.pos),
            format!(
                "{}: xs:import of namespace {ns:?} has no schemaLocation and no supplied \
                 schema defines that namespace{hint}",
                rf.from
            ),
        );
        w.diags.push(d);
    }
}
