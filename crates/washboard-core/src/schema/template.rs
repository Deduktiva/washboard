//! Request body templates (PLAN "Template generation", §5.1).
//!
//! The output is meant to be edited, not sent as is: placeholders stand in for values, and
//! comments say where the schema offers choices. After replacing the placeholders that the
//! types' facets cannot satisfy (patterns), the result should validate.

use std::collections::{HashMap, HashSet};

use crate::model::QName;
use crate::soap::XSI_NS;
use crate::xml::{escape_attr, escape_text};

use super::builtin::ANY_TYPE;
use super::component::{ElemId, NamespaceConstraint, Particle, ProcessContents, Term, Wildcard};
use super::{MAX_CHAIN, SchemaError, SchemaModel, TypeKey};

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOptions {
    /// Element nesting below the root after which content is replaced by a truncation
    /// comment. Bounds recursive types.
    pub max_depth: usize,
    /// Maximum number of elements emitted; bounds very wide schemas.
    pub max_nodes: usize,
    pub indent: String,
    /// Preferred `(prefix, namespace)` pairs, e.g. those the surrounding envelope declares.
    /// Other namespaces get the prefix used in the schema documents, or a derived one.
    pub prefixes: Vec<(String, String)>,
    /// Declare the used namespaces on the emitted root element. Turn off when the caller
    /// declares [`Template::namespaces`] on an enclosing element instead.
    pub declare_namespaces: bool,
}

impl Default for TemplateOptions {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_nodes: 2000,
            indent: "  ".into(),
            prefixes: Vec::new(),
            declare_namespaces: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// The element (possibly preceded by a comment), without a trailing newline. Lines after
    /// the first are indented relative to the root at column 0.
    pub xml: String,
    /// `(prefix, namespace)` for every namespace the template uses, in order of first use.
    pub namespaces: Vec<(String, String)>,
    /// The depth limit or node budget cut something off (marked by a comment).
    pub truncated: bool,
}

impl SchemaModel {
    /// Generates an instance template for a global element.
    pub fn template(
        &self,
        element: &QName,
        opts: &TemplateOptions,
    ) -> Result<Template, SchemaError> {
        let &id = self
            .global_elements
            .get(element)
            .ok_or_else(|| SchemaError::UnknownElement(element.clone()))?;
        let mut g = Gen {
            m: self,
            opts,
            px: Prefixes::new(self, opts),
            nodes: 0,
            truncated: false,
            budget_reported: false,
            group_depth: 0,
        };
        let mut top = Vec::new();
        g.element(id, Vec::new(), 0, &mut top);
        let mut xml = String::new();
        let mut declared = !opts.declare_namespaces;
        for n in &top {
            let decls = if !declared && matches!(n, Node::Elem { .. }) {
                declared = true;
                g.px.used.as_slice()
            } else {
                &[]
            };
            write_node(n, 0, &opts.indent, decls, &mut xml);
        }
        while xml.ends_with('\n') {
            xml.pop();
        }
        Ok(Template {
            xml,
            namespaces: g.px.used,
            truncated: g.truncated,
        })
    }
}

#[derive(Debug)]
enum Node {
    Elem {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<Node>,
        text: Option<String>,
    },
    Comment(String),
}

struct Gen<'a> {
    m: &'a SchemaModel,
    opts: &'a TemplateOptions,
    px: Prefixes,
    nodes: usize,
    truncated: bool,
    budget_reported: bool,
    group_depth: usize,
}

impl Gen<'_> {
    fn q(&mut self, q: &QName) -> String {
        self.px.format(q)
    }

    fn list(&mut self, names: impl IntoIterator<Item = QName>) -> String {
        let v: Vec<String> = names.into_iter().map(|q| self.q(&q)).collect();
        v.join(", ")
    }

    fn type_label(&mut self, key: TypeKey) -> String {
        let px = &mut self.px;
        self.m.type_label(key, &mut |q| px.format(q))
    }

    fn element(&mut self, id: ElemId, mut markers: Vec<String>, depth: usize, out: &mut Vec<Node>) {
        let m = self.m;
        if self.nodes >= self.opts.max_nodes {
            self.truncated = true;
            if !self.budget_reported {
                self.budget_reported = true;
                out.push(Node::Comment(format!(
                    "… truncated: template limit of {} elements reached",
                    self.opts.max_nodes
                )));
            }
            return;
        }
        let mut id = id;
        let head = m.elem(id);
        if head.is_abstract {
            let members: Vec<ElemId> = m
                .substitution_members(id)
                .into_iter()
                .filter(|&e| !m.elem(e).is_abstract)
                .collect();
            let head_name = self.q(&head.name);
            let Some((&first, rest)) = members.split_first() else {
                out.push(Node::Comment(format!(
                    "abstract element {head_name} has no concrete substitution group member"
                )));
                return;
            };
            id = first;
            let alts = self.list(rest.iter().map(|&e| m.elem(e).name.clone()));
            markers.push(if alts.is_empty() {
                format!("substitutes abstract {head_name}")
            } else {
                format!("substitutes abstract {head_name}; alternatives: {alts}")
            });
        }
        let e = m.elem(id);
        let declared = m.elem_type(id);
        let mut xsi = None;
        if m.is_abstract_type(declared) {
            let options = m.xsi_type_options(declared, e.block);
            match options.split_first() {
                Some((&first, rest)) => {
                    xsi = Some(first);
                    let alts = self.list(rest.iter().filter_map(|&k| m.type_name(k)));
                    if !alts.is_empty() {
                        markers.push(format!("xsi:type alternatives: {alts}"));
                    }
                }
                None => {
                    let label = self.type_label(declared);
                    markers.push(format!(
                        "abstract type {label} has no concrete derived type"
                    ));
                }
            }
        }
        let effective = xsi.unwrap_or(declared);
        self.nodes += 1;

        let name = self.q(&e.name);
        let mut attrs = Vec::new();
        if let Some(x) = xsi
            && let Some(tn) = m.type_name(x)
        {
            let attr = self.q(&QName::new(XSI_NS, "type"));
            attrs.push((attr, self.q(&tn)));
        }
        for a in m
            .effective_attributes(effective)
            .0
            .into_iter()
            .filter(|a| a.required)
        {
            let value = a
                .fixed
                .or(a.default)
                .unwrap_or_else(|| m.placeholder(a.type_key));
            attrs.push((self.q(&a.name), value));
        }

        let mut children = Vec::new();
        let mut text = None;
        if m.has_simple_value(effective) {
            text = Some(
                e.fixed
                    .clone()
                    .or_else(|| e.default.clone())
                    .unwrap_or_else(|| m.placeholder(effective)),
            );
        } else {
            let particles = m.content_particles(effective);
            if !particles.is_empty() {
                if depth >= self.opts.max_depth {
                    self.truncated = true;
                    let label = self.type_label(effective);
                    children.push(Node::Comment(format!("… truncated: type {label}")));
                } else {
                    for p in particles {
                        self.particle(p, depth + 1, false, &mut children);
                    }
                }
            }
            if children.is_empty()
                && (m.is_mixed(effective) || effective == TypeKey::Builtin(ANY_TYPE))
            {
                text = Some("?".into());
            }
        }
        if !markers.is_empty() {
            out.push(Node::Comment(markers.join("; ")));
        }
        out.push(Node::Elem {
            name,
            attrs,
            children,
            text,
        });
    }

    fn particle(&mut self, p: &Particle, depth: usize, optional: bool, out: &mut Vec<Node>) {
        if p.max.is_zero() || self.group_depth > MAX_CHAIN {
            return;
        }
        let optional = optional || p.min == 0;
        let count = if optional { 1 } else { p.min.max(1) };
        for _ in 0..count {
            if self.budget_reported {
                return;
            }
            let markers = || {
                if optional {
                    vec!["optional".to_string()]
                } else {
                    Vec::new()
                }
            };
            match &p.term {
                Term::Element(id) => self.element(*id, markers(), depth, out),
                Term::ElementRef(q) => {
                    if let Some(&id) = self.m.global_elements.get(q) {
                        self.element(id, markers(), depth, out);
                    }
                }
                Term::GroupRef(q) => {
                    if let Some(gp) = self.m.groups.get(q).and_then(|g| g.particle.as_ref()) {
                        self.group_depth += 1;
                        self.particle(gp, depth, optional, out);
                        self.group_depth -= 1;
                    }
                }
                Term::Sequence(c) | Term::All(c) => {
                    self.group_depth += 1;
                    for cp in c {
                        self.particle(cp, depth, optional, out);
                    }
                    self.group_depth -= 1;
                }
                Term::Choice(c) => {
                    let Some((first, rest)) = c.split_first() else {
                        continue;
                    };
                    if !rest.is_empty() {
                        let alts: Vec<String> = rest.iter().map(|b| self.describe(b, 0)).collect();
                        out.push(Node::Comment(format!(
                            "{}choice; alternatives: {}",
                            if optional { "optional " } else { "" },
                            alts.join(", ")
                        )));
                    }
                    self.group_depth += 1;
                    self.particle(first, depth, optional, out);
                    self.group_depth -= 1;
                }
                Term::Any(w) => {
                    let c = self.any_comment(w);
                    out.push(Node::Comment(c));
                }
            }
        }
    }

    /// A short description of a choice branch.
    fn describe(&mut self, p: &Particle, nest: usize) -> String {
        let m = self.m;
        let inner = |g: &mut Self, c: &[Particle]| {
            if nest > 2 {
                return "…".to_string();
            }
            let mut v: Vec<String> = c
                .iter()
                .take(3)
                .map(|cp| g.describe(cp, nest + 1))
                .collect();
            if c.len() > 3 {
                v.push("…".into());
            }
            v.join(", ")
        };
        match &p.term {
            Term::Element(id) => self.q(&m.elem(*id).name),
            Term::ElementRef(q) => self.q(q),
            Term::GroupRef(q) => format!("group {}", self.q(q)),
            Term::Sequence(c) | Term::All(c) => format!("({})", inner(self, c)),
            Term::Choice(c) => format!("choice({})", inner(self, c)),
            Term::Any(_) => "any element".into(),
        }
    }

    fn any_comment(&mut self, w: &Wildcard) -> String {
        let from = match &w.namespaces {
            NamespaceConstraint::Any => "any namespace".to_string(),
            NamespaceConstraint::Not(tns) if tns.is_empty() => "any namespace".to_string(),
            NamespaceConstraint::Not(tns) => format!("any namespace except {tns}"),
            NamespaceConstraint::List(l) => l
                .iter()
                .map(|n| {
                    if n.is_empty() {
                        "no namespace"
                    } else {
                        n.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        };
        let mut s = format!("any element from: {from}");
        match w.process_contents {
            ProcessContents::Skip => s.push_str(" (not validated)"),
            ProcessContents::Lax => s.push_str(" (lax)"),
            ProcessContents::Strict => {}
        }
        if w.process_contents != ProcessContents::Skip {
            let known = self.m.wildcard_elements(w);
            if !known.is_empty() {
                let names: Vec<QName> = known
                    .iter()
                    .take(5)
                    .map(|&id| self.m.elem(id).name.clone())
                    .collect();
                let more = if known.len() > 5 { ", …" } else { "" };
                s.push_str(&format!("; known: {}{more}", self.list(names)));
            }
        }
        s
    }
}

/// Assigns one prefix per namespace, stable for the whole template.
struct Prefixes {
    preferred: Vec<(String, String)>,
    by_ns: HashMap<String, String>,
    taken: HashSet<String>,
    used: Vec<(String, String)>,
    counter: usize,
}

impl Prefixes {
    fn new(m: &SchemaModel, opts: &TemplateOptions) -> Self {
        let mut preferred: Vec<(String, String)> = opts.prefixes.clone();
        preferred.extend(m.prefix_hints.iter().map(|(ns, p)| (p.clone(), ns.clone())));
        Self {
            preferred,
            by_ns: HashMap::new(),
            taken: HashSet::new(),
            used: Vec::new(),
            counter: 0,
        }
    }

    fn format(&mut self, q: &QName) -> String {
        match self.prefix(&q.ns) {
            Some(p) => format!("{p}:{}", q.local),
            None => q.local.clone(),
        }
    }

    fn usable(&self, p: &str, ns: &str) -> bool {
        is_ncname(p)
            && !p.to_ascii_lowercase().starts_with("xml")
            && !self.taken.contains(p)
            && (p != "xsi" || ns == XSI_NS)
    }

    fn prefix(&mut self, ns: &str) -> Option<String> {
        if ns.is_empty() {
            return None;
        }
        if ns == XML_NS {
            return Some("xml".into());
        }
        if let Some(p) = self.by_ns.get(ns) {
            return Some(p.clone());
        }
        let mut chosen = None;
        if ns == XSI_NS && self.usable("xsi", ns) {
            chosen = Some("xsi".to_string());
        }
        if chosen.is_none() {
            chosen = self
                .preferred
                .iter()
                .find(|(p, n)| n == ns && self.usable(p, ns))
                .map(|(p, _)| p.clone());
        }
        if chosen.is_none() {
            let seg = ns
                .rsplit([':', '/', '#'])
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_ascii_lowercase();
            if seg.len() <= 12 && self.usable(&seg, ns) {
                chosen = Some(seg);
            }
        }
        let p = match chosen {
            Some(p) => p,
            None => loop {
                self.counter += 1;
                let p = format!("ns{}", self.counter);
                if self.usable(&p, ns) {
                    break p;
                }
            },
        };
        self.taken.insert(p.clone());
        self.by_ns.insert(ns.to_string(), p.clone());
        self.used.push((p.clone(), ns.to_string()));
        Some(p)
    }
}

fn is_ncname(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn comment_text(s: &str) -> String {
    let mut t = s.replace("--", "- -");
    if t.ends_with('-') {
        t.push(' ');
    }
    t
}

fn write_node(n: &Node, level: usize, indent: &str, decls: &[(String, String)], out: &mut String) {
    let pad = indent.repeat(level);
    match n {
        Node::Comment(t) => {
            out.push_str(&format!("{pad}<!-- {} -->\n", comment_text(t)));
        }
        Node::Elem {
            name,
            attrs,
            children,
            text,
        } => {
            out.push_str(&pad);
            out.push('<');
            out.push_str(name);
            for (p, ns) in decls {
                out.push_str(&format!(" xmlns:{p}=\"{}\"", escape_attr(ns)));
            }
            for (a, v) in attrs {
                out.push_str(&format!(" {a}=\"{}\"", escape_attr(v)));
            }
            match (text, children.is_empty()) {
                (Some(t), true) => out.push_str(&format!(">{}</{name}>\n", escape_text(t))),
                (None, true) => out.push_str("/>\n"),
                _ => {
                    out.push_str(">\n");
                    for c in children {
                        write_node(c, level + 1, indent, &[], out);
                    }
                    out.push_str(&format!("{pad}</{name}>\n"));
                }
            }
        }
    }
}
