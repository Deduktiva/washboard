//! Builds the [`SchemaBundle`] (`docs/PLAN.md` §5 steps 1–2 and 5, §5.4).
//!
//! libxml2 imports each target namespace only once: a second `xs:import` of a namespace
//! with a different location is skipped silently. So every namespace gets exactly one
//! *canonical* document, and every `xs:import` of that namespace anywhere in the bundle is
//! rewritten to point at it:
//! - one import target (inline schema, or file reached through an import): that document;
//! - several (namespace split across files, or several inline schemas sharing a namespace):
//!   a generated wrapper that `xs:include`s all of them;
//! - rpc/literal wrappers in that namespace: a generated document holding the wrappers that
//!   also `xs:include`s the real documents (PLAN §5.4 gotcha).
//!
//! The root imports every canonical document once.
//!
//! URIs are absolute (`washboard:/…`) so libxml2's base-URI resolution passes them through
//! unchanged. Relative URIs would be resolved against the including document and miss.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::diag::{DiagSource, Diagnostic};
use crate::model::{QName, SchemaBundle, SchemaDoc, SchemaOrigin};
use crate::soap::XSD_NS;

use super::defs::{Definitions, Direction, Operation, PartContent, Protocol, Style};
use super::graph::{FileKind, Owner, RefKind, Walk};
use super::source::percent_encode_path;
use super::text::{Edit, splice, xml_decl_encoding};
use crate::diag::LineIndex;
use crate::xml::{escape_attr, parse_wsdl_or_xsd, start_tag_at};

/// URI prefix of supplied files inside the bundle; the rest is the percent-encoded
/// project-relative path.
pub const FILE_URI_PREFIX: &str = "washboard:/wsdl/";
pub const ROOT_URI: &str = "washboard:/root.xsd";
/// Target namespace of the generated root; it must differ from every real namespace and
/// be non-empty so the root may import no-namespace schemas.
pub const ROOT_NS: &str = "urn:washboard:root";

/// Bundle URI for a supplied file at project-relative `path`.
pub fn file_uri(path: &str) -> String {
    format!("{FILE_URI_PREFIX}{}", percent_encode_path(path))
}

/// A namespace whose components come from several separately imported documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitNamespace {
    pub namespace: String,
    /// Display labels: project-relative paths, `path (inline schema n)` for inline ones.
    pub documents: Vec<String>,
    /// URI of the generated document that includes all of them.
    pub combined_as: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    File(usize),
    Inline(usize),
}

pub(crate) struct BundleOut {
    pub bundle: SchemaBundle,
    pub splits: Vec<SplitNamespace>,
    pub rpc_wrappers: usize,
}

struct RpcWrapper {
    name: String,
    /// (part name, content) in order.
    parts: Vec<(String, PartContent)>,
}

pub(crate) fn build(
    w: &Walk,
    names: &[String],
    defs: &Definitions,
    diags: &mut Vec<Diagnostic>,
) -> BundleOut {
    // ---- nodes, URIs, namespaces
    let mut nodes: Vec<Node> = Vec::new();
    for (i, _) in w.inline.iter().enumerate() {
        nodes.push(Node::Inline(i));
    }
    for &f in &w.xsd_order {
        nodes.push(Node::File(f));
    }
    let uri = |n: Node| match n {
        Node::File(f) => file_uri(&names[f]),
        Node::Inline(i) => format!("washboard:/inline/{i}.xsd"),
    };
    let tns = |n: Node| -> &str {
        match n {
            Node::File(f) => &w.files[f].tns,
            Node::Inline(i) => &w.inline[i].tns,
        }
    };
    let label = |n: Node| match n {
        Node::File(f) => names[f].clone(),
        Node::Inline(i) => format!(
            "{} (inline schema {})",
            names[w.inline[i].file],
            w.inline[i].index + 1
        ),
    };
    let owner_node = |o: Owner| match o {
        Owner::File(f) => Node::File(f),
        Owner::Inline(i) => Node::Inline(i),
    };

    // ---- include edges and import targets
    let mut include_target: HashMap<usize, Node> = HashMap::new();
    let mut includes: HashMap<Node, Vec<Node>> = HashMap::new();
    let mut targets: Vec<Node> = nodes
        .iter()
        .copied()
        .filter(|n| matches!(n, Node::Inline(_)))
        .collect();
    for (r, site) in w.sites.iter().enumerate() {
        let Some(t) = site.target else { continue };
        let kind = w.refs[r].kind;
        let from = owner_node(site.owner);
        match (kind, w.files[t].kind) {
            (RefKind::XsdImport | RefKind::WsdlImport, FileKind::Xsd) => {
                if !targets.contains(&Node::File(t)) {
                    targets.push(Node::File(t));
                }
            }
            (RefKind::XsdInclude | RefKind::XsdRedefine | RefKind::XsdOverride, k) => {
                let to = match k {
                    FileKind::Xsd => Some(Node::File(t)),
                    FileKind::Wsdl => {
                        let own = tns(from);
                        let cands: Vec<usize> = (0..w.inline.len())
                            .filter(|&i| w.inline[i].file == t && w.inline[i].tns == own)
                            .collect();
                        if cands.len() != 1 {
                            diags.push(Diagnostic::warning(
                                DiagSource::Import,
                                Some(w.refs[r].pos),
                                format!(
                                    "{}: {} of a WSDL needs exactly one inline schema with \
                                     namespace {own:?} in {}, found {}",
                                    w.refs[r].from,
                                    kind.label(),
                                    names[t],
                                    cands.len()
                                ),
                            ));
                        }
                        cands.first().map(|&i| Node::Inline(i))
                    }
                    _ => None,
                };
                if let Some(to) = to {
                    include_target.insert(r, to);
                    includes.entry(from).or_default().push(to);
                }
            }
            _ => {}
        }
    }
    let reaches = |a: Node, b: Node| -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![a];
        while let Some(n) = stack.pop() {
            for &m in includes.get(&n).map_or(&[][..], Vec::as_slice) {
                if m == b {
                    return true;
                }
                if seen.insert(m) {
                    stack.push(m);
                }
            }
        }
        false
    };
    // Group by namespace, preserving first-seen order; drop targets that another kept
    // target already includes (they come along with it).
    let mut ns_order: Vec<String> = Vec::new();
    let mut by_ns: HashMap<String, Vec<Node>> = HashMap::new();
    for t in targets {
        let ns = tns(t).to_owned();
        let kept = by_ns.entry(ns.clone()).or_insert_with(|| {
            ns_order.push(ns.clone());
            Vec::new()
        });
        if kept.iter().any(|&k| reaches(k, t)) {
            continue;
        }
        kept.retain(|&k| !reaches(t, k));
        kept.push(t);
    }

    // ---- rpc wrappers per namespace
    let rpc = rpc_wrappers(defs, diags);
    for ns in rpc.keys() {
        if !by_ns.contains_key(ns) {
            ns_order.push(ns.clone());
            by_ns.insert(ns.clone(), Vec::new());
        }
    }

    // ---- canonical documents
    let mut canonical: HashMap<String, String> = HashMap::new();
    let mut generated_ns: Vec<(String, String)> = Vec::new(); // (ns, uri) needing a wrapper
    let mut splits = Vec::new();
    for (k, ns) in ns_order.iter().enumerate() {
        let kept = &by_ns[ns];
        let c = if rpc.contains_key(ns) {
            format!("washboard:/rpc/{k}.xsd")
        } else if kept.len() == 1 {
            uri(kept[0])
        } else {
            format!("washboard:/ns/{k}.xsd")
        };
        if kept.len() > 1 {
            splits.push(SplitNamespace {
                namespace: ns.clone(),
                documents: kept.iter().map(|&n| label(n)).collect(),
                combined_as: c.clone(),
            });
        }
        if rpc.contains_key(ns) || kept.len() > 1 {
            generated_ns.push((ns.clone(), c.clone()));
        }
        canonical.insert(ns.clone(), c);
    }

    // ---- rewritten documents
    let mut docs = Vec::new();
    let mut parsed_globals: HashMap<String, HashSet<String>> = HashMap::new();
    for &n in &nodes {
        let mut edits = Vec::new();
        for (r, site) in w.sites.iter().enumerate() {
            if owner_node(site.owner) != n {
                continue;
            }
            let rf = &w.refs[r];
            let new_loc = match rf.kind {
                RefKind::WsdlImport => None,
                RefKind::XsdImport => {
                    let ns = match site.target {
                        Some(t) if w.files[t].kind == FileKind::Xsd => w.files[t].tns.as_str(),
                        _ => rf.namespace.as_deref().unwrap_or(""),
                    };
                    canonical.get(ns).cloned()
                }
                _ => include_target.get(&r).map(|&t| uri(t)),
            };
            if let Some(loc) = new_loc {
                edits.push(match &site.loc_value {
                    Some(range) => Edit {
                        range: range.clone(),
                        text: escape_attr(&loc).into_owned(),
                    },
                    None => Edit {
                        range: site.name_end..site.name_end,
                        text: format!(" schemaLocation=\"{}\"", escape_attr(&loc)),
                    },
                });
            }
        }
        let (text, origin) = match n {
            Node::File(f) => {
                let Some(src) = w.files[f].text.as_deref() else {
                    continue;
                };
                if let Some(r) = xml_decl_encoding(src)
                    && !src[r.clone()].eq_ignore_ascii_case("utf-8")
                {
                    // The text is UTF-8 now; a stale declaration would make libxml2
                    // decode it wrongly.
                    edits.push(Edit {
                        range: r,
                        text: "UTF-8".into(),
                    });
                }
                (
                    splice(src, 0..src.len(), edits),
                    SchemaOrigin::File {
                        path: names[f].clone(),
                    },
                )
            }
            Node::Inline(i) => {
                let s = &w.inline[i];
                let Some(src) = w.files[s.file].text.as_deref() else {
                    continue;
                };
                let Some(text) = extract_inline(src, s.range.clone(), edits) else {
                    continue;
                };
                (
                    text,
                    SchemaOrigin::InlineWsdl {
                        wsdl_path: names[s.file].clone(),
                        index: s.index,
                    },
                )
            }
        };
        if rpc.contains_key(tns(n)) {
            parsed_globals
                .entry(tns(n).to_owned())
                .or_default()
                .extend(global_elements(&text));
        }
        docs.push(SchemaDoc {
            uri: uri(n),
            target_ns: tns(n).to_owned(),
            origin,
            text,
        });
    }

    // ---- generated wrappers
    let mut rpc_count = 0;
    for (ns, c) in &generated_ns {
        let mut g = GenSchema::new(ns);
        for &n in &by_ns[ns] {
            g.include(&uri(n));
        }
        if let Some(wrappers) = rpc.get(ns) {
            let globals = parsed_globals.get(ns);
            for wr in wrappers {
                if globals.is_some_and(|g| g.contains(&wr.name)) {
                    diags.push(Diagnostic::warning(
                        DiagSource::Import,
                        None,
                        format!(
                            "rpc wrapper element {} is already declared by a schema; the \
                             schema's declaration is used",
                            QName::new(ns.clone(), wr.name.clone())
                        ),
                    ));
                    continue;
                }
                rpc_count += 1;
                g.wrapper(wr, &canonical, diags);
            }
        }
        docs.push(SchemaDoc {
            uri: c.clone(),
            target_ns: ns.clone(),
            origin: SchemaOrigin::Generated,
            text: g.finish(),
        });
    }

    // ---- root
    let mut root = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    root.push_str(&format!(
        "<xs:schema xmlns:xs=\"{XSD_NS}\" targetNamespace=\"{ROOT_NS}\">\n"
    ));
    for ns in &ns_order {
        let c = &canonical[ns];
        if ns.is_empty() {
            root.push_str(&format!(
                "  <xs:import schemaLocation=\"{}\"/>\n",
                escape_attr(c)
            ));
        } else {
            root.push_str(&format!(
                "  <xs:import namespace=\"{}\" schemaLocation=\"{}\"/>\n",
                escape_attr(ns),
                escape_attr(c)
            ));
        }
    }
    root.push_str("</xs:schema>\n");
    docs.push(SchemaDoc {
        uri: ROOT_URI.to_owned(),
        target_ns: ROOT_NS.to_owned(),
        origin: SchemaOrigin::Generated,
        text: root,
    });

    BundleOut {
        bundle: SchemaBundle {
            docs,
            root: ROOT_URI.to_owned(),
        },
        splits,
        rpc_wrappers: rpc_count,
    }
}

/// Cuts the `xs:schema` element at `range` out of a WSDL, applying `edits` and declaring
/// every namespace in scope there that the element does not declare itself (the classic
/// extraction bug: `type="tns:Foo"` with `tns` declared on `wsdl:definitions`).
///
/// The excerpt is preceded by newlines and spaces so that line and column numbers inside it
/// equal those in the WSDL; libxml2 errors about inline schemas then point into the WSDL.
fn extract_inline(
    src: &str,
    range: std::ops::Range<usize>,
    mut edits: Vec<Edit>,
) -> Option<String> {
    let doc = parse_wsdl_or_xsd(src).ok()?;
    let el = doc
        .descendants()
        .find(|n| n.is_element() && n.range() == range)?;
    let tag = start_tag_at(src, range.start)?;
    let declared: HashSet<Option<&str>> = tag
        .attr_names
        .iter()
        .map(|r| &src[r.clone()])
        .filter_map(|a| {
            if a == "xmlns" {
                Some(None)
            } else {
                a.strip_prefix("xmlns:").map(Some)
            }
        })
        .collect();
    let mut decls = String::new();
    for ns in el.namespaces() {
        if ns.name() == Some("xml") || declared.contains(&ns.name()) {
            continue;
        }
        match ns.name() {
            Some(p) => decls.push_str(&format!(" xmlns:{p}=\"{}\"", escape_attr(ns.uri()))),
            None => decls.push_str(&format!(" xmlns=\"{}\"", escape_attr(ns.uri()))),
        }
    }
    if !decls.is_empty() {
        edits.push(Edit {
            range: tag.name.end..tag.name.end,
            text: decls,
        });
    }
    let pos = LineIndex::new(src).pos(range.start);
    let mut out = String::new();
    for _ in 1..pos.line {
        out.push('\n');
    }
    for _ in 1..pos.column {
        out.push(' ');
    }
    out.push_str(&splice(src, range, edits));
    Some(out)
}

fn global_elements(text: &str) -> Vec<String> {
    let Ok(doc) = parse_wsdl_or_xsd(text) else {
        return Vec::new();
    };
    doc.root_element()
        .children()
        .filter(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(XSD_NS)
                && n.tag_name().name() == "element"
        })
        .filter_map(|n| n.attribute("name").map(str::to_owned))
        .collect()
}

/// rpc/literal wrapper declarations, grouped by `soap:body namespace`.
fn rpc_wrappers(
    defs: &Definitions,
    diags: &mut Vec<Diagnostic>,
) -> BTreeMap<String, Vec<RpcWrapper>> {
    let mut out: BTreeMap<String, Vec<RpcWrapper>> = BTreeMap::new();
    for b in &defs.bindings {
        if b.protocol != Protocol::Soap11 {
            continue;
        }
        for op in b
            .operations
            .iter()
            .filter(|o| o.is_supported() && o.style == Style::Rpc)
        {
            for dir in [Direction::Input, Direction::Output] {
                add_wrapper(&mut out, b.name.local.as_str(), op, dir, diags);
            }
        }
    }
    out
}

fn add_wrapper(
    out: &mut BTreeMap<String, Vec<RpcWrapper>>,
    binding: &str,
    op: &Operation,
    dir: Direction,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(m) = op.message(dir) else { return };
    let q = op.rpc_wrapper(dir);
    let parts: Vec<(String, PartContent)> = m
        .body_parts
        .iter()
        .map(|p| (p.name.clone(), p.content.clone()))
        .collect();
    let list = out.entry(q.ns.clone()).or_default();
    if let Some(existing) = list.iter().find(|w| w.name == q.local) {
        if existing.parts != parts {
            diags.push(Diagnostic::warning(
                DiagSource::Import,
                None,
                format!(
                    "binding {binding}: rpc wrapper {q} is generated by several operations \
                     with different parts; the first one is used"
                ),
            ));
        }
        return;
    }
    list.push(RpcWrapper {
        name: q.local,
        parts,
    });
}

/// Writer for generated schema documents.
struct GenSchema {
    tns: String,
    prefixes: BTreeMap<String, String>,
    body: String,
    /// (namespace, canonical URI) of namespaces referenced by wrapper parts.
    imports: Vec<(String, String)>,
}

impl GenSchema {
    fn new(tns: &str) -> Self {
        Self {
            tns: tns.to_owned(),
            prefixes: BTreeMap::new(),
            body: String::new(),
            imports: Vec::new(),
        }
    }

    fn include(&mut self, uri: &str) {
        self.body.push_str(&format!(
            "  <xs:include schemaLocation=\"{}\"/>\n",
            escape_attr(uri)
        ));
    }

    /// A QName reference usable in an attribute value. No-namespace names stay unprefixed;
    /// the document never declares a default namespace.
    fn qref(
        &mut self,
        q: &QName,
        canonical: &HashMap<String, String>,
        diags: &mut Vec<Diagnostic>,
    ) -> String {
        if q.ns == XSD_NS {
            return format!("xs:{}", q.local);
        }
        if q.ns != self.tns && !self.imports.iter().any(|(ns, _)| *ns == q.ns) {
            if let Some(c) = canonical.get(&q.ns) {
                self.imports.push((q.ns.clone(), c.clone()));
            } else {
                diags.push(Diagnostic::warning(
                    DiagSource::Import,
                    None,
                    format!("rpc part type {q} is in a namespace no supplied schema defines"),
                ));
            }
        }
        if q.ns.is_empty() {
            return q.local.clone();
        }
        let n = self.prefixes.len();
        let p = self
            .prefixes
            .entry(q.ns.clone())
            .or_insert_with(|| format!("n{n}"));
        format!("{p}:{}", q.local)
    }

    fn wrapper(
        &mut self,
        w: &RpcWrapper,
        canonical: &HashMap<String, String>,
        diags: &mut Vec<Diagnostic>,
    ) {
        let mut s = format!(
            "  <xs:element name=\"{}\">\n    <xs:complexType>\n      <xs:sequence>\n",
            escape_attr(&w.name)
        );
        for (name, content) in &w.parts {
            // Parts are unqualified local elements (no elementFormDefault here).
            match content {
                PartContent::Type(t) => {
                    let r = self.qref(t, canonical, diags);
                    s.push_str(&format!(
                        "        <xs:element name=\"{}\" type=\"{}\"/>\n",
                        escape_attr(name),
                        escape_attr(&r)
                    ));
                }
                PartContent::Element(e) => {
                    let r = self.qref(e, canonical, diags);
                    s.push_str(&format!(
                        "        <xs:element ref=\"{}\"/>\n",
                        escape_attr(&r)
                    ));
                }
                PartContent::Missing => {
                    s.push_str(&format!(
                        "        <xs:element name=\"{}\"/>\n",
                        escape_attr(name)
                    ));
                }
            }
        }
        s.push_str("      </xs:sequence>\n    </xs:complexType>\n  </xs:element>\n");
        self.body.push_str(&s);
    }

    fn finish(self) -> String {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str(&format!("<xs:schema xmlns:xs=\"{XSD_NS}\""));
        for (ns, p) in &self.prefixes {
            out.push_str(&format!(" xmlns:{p}=\"{}\"", escape_attr(ns)));
        }
        if !self.tns.is_empty() {
            out.push_str(&format!(" targetNamespace=\"{}\"", escape_attr(&self.tns)));
        }
        out.push_str(">\n");
        for (ns, uri) in &self.imports {
            if ns.is_empty() {
                out.push_str(&format!(
                    "  <xs:import schemaLocation=\"{}\"/>\n",
                    escape_attr(uri)
                ));
            } else {
                out.push_str(&format!(
                    "  <xs:import namespace=\"{}\" schemaLocation=\"{}\"/>\n",
                    escape_attr(ns),
                    escape_attr(uri)
                ));
            }
        }
        out.push_str(&self.body);
        out.push_str("</xs:schema>\n");
        out
    }
}
