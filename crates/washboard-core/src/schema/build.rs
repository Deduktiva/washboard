//! Builds a [`SchemaModel`] from a [`SchemaBundle`].
//!
//! Every document of the bundle is processed with its own target namespace, except
//! no-namespace documents that are only reached through `xs:include`/`xs:redefine`: those are
//! chameleons and are processed once per including namespace instead. Imports need no walking
//! because the bundle is closed and every document is processed anyway.

use std::collections::{BTreeSet, HashMap, HashSet};

use roxmltree::{Document, Node};

use crate::model::{QName, SchemaBundle};
use crate::soap::XSD_NS;

use super::component::{
    AttrGroupDef, AttrItem, AttrUse, AttributeDecl, BaseRef, Content, Derivation, DerivationSet,
    ElemId, ElementDecl, Facets, GroupDef, MaxOccurs, NamespaceConstraint, Particle,
    ProcessContents, SimpleVariety, Term, TypeDef, TypeId, TypeKind, TypeRef, Wildcard,
};
use super::{SchemaModel, SchemaWarning, TypeKey};

/// Per-document parsing context.
struct Ctx<'a> {
    uri: &'a str,
    doc: &'a Document<'a>,
    /// Effective target namespace (the includer's for a chameleon document).
    tns: String,
    chameleon: bool,
    element_qualified: bool,
    attribute_qualified: bool,
    block_default: DerivationSet,
    final_default: DerivationSet,
}

struct Builder {
    m: SchemaModel,
    done: HashSet<(usize, String)>,
    hinted: HashSet<String>,
}

impl SchemaModel {
    /// Builds the model. Never fails: problems are collected in [`SchemaModel::warnings`].
    pub fn build(bundle: &SchemaBundle) -> SchemaModel {
        let mut b = Builder {
            m: SchemaModel::default(),
            done: HashSet::new(),
            hinted: HashSet::new(),
        };
        let docs: Vec<Option<Document<'_>>> = bundle
            .docs
            .iter()
            .map(|d| match crate::xml::parse_wsdl_or_xsd(&d.text) {
                Ok(doc) => Some(doc),
                Err(e) => {
                    b.m.warnings.push(SchemaWarning {
                        uri: d.uri.clone(),
                        pos: Some(e.pos().into()),
                        message: format!("not well-formed: {e}"),
                    });
                    None
                }
            })
            .collect();
        let by_uri: HashMap<&str, usize> = bundle
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.uri.as_str(), i))
            .collect();

        // Documents reached by include/redefine; no-namespace ones among them are chameleons.
        let mut included = vec![false; docs.len()];
        for (i, doc) in docs.iter().enumerate() {
            let Some(doc) = doc else { continue };
            for c in xs_children(doc.root_element()) {
                if matches!(c.tag_name().name(), "include" | "redefine")
                    && let Some(t) = c
                        .attribute("schemaLocation")
                        .and_then(|l| locate(&by_uri, &bundle.docs[i].uri, l))
                {
                    included[t] = true;
                }
            }
        }
        for (i, doc) in docs.iter().enumerate() {
            let Some(doc) = doc else { continue };
            let own_tns = doc
                .root_element()
                .attribute("targetNamespace")
                .unwrap_or("");
            if own_tns.is_empty() && included[i] {
                continue;
            }
            b.process(bundle, &docs, &by_uri, i, own_tns.to_string());
        }
        b.finish()
    }
}

fn is_xs(n: Node<'_, '_>) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(XSD_NS)
}

fn xs_children<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children()
        .filter(|c| is_xs(*c) && c.tag_name().name() != "annotation")
}

fn xs_child<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    xs_children(n).find(|c| c.tag_name().name() == local)
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("true" | "1"))
}

/// Resolves a `schemaLocation` to a bundle document: as written (WP-WSDL rewrites locations to
/// bundle URIs), else relative to the referencing document's URI.
fn locate(by_uri: &HashMap<&str, usize>, from: &str, loc: &str) -> Option<usize> {
    let loc = loc.trim();
    if let Some(&i) = by_uri.get(loc) {
        return Some(i);
    }
    let dir = match from.rfind('/') {
        Some(p) => &from[..=p],
        None => "",
    };
    let joined = format!("{dir}{loc}");
    let (scheme, path) = match joined.find(":/") {
        Some(p) => joined.split_at(p + 2),
        None => ("", joined.as_str()),
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "." => {}
            ".." if parts.last().is_some_and(|s| *s != "..") => {
                parts.pop();
            }
            _ => parts.push(seg),
        }
    }
    by_uri
        .get(format!("{scheme}{}", parts.join("/")).as_str())
        .copied()
}

/// The text of all `xs:annotation/xs:documentation` children, indentation removed.
fn documentation(n: Node<'_, '_>) -> Option<String> {
    let mut blocks = Vec::new();
    for ann in n
        .children()
        .filter(|c| is_xs(*c) && c.tag_name().name() == "annotation")
    {
        for d in ann
            .children()
            .filter(|c| is_xs(*c) && c.tag_name().name() == "documentation")
        {
            let text: String = d
                .descendants()
                .filter(|t| t.is_text())
                .filter_map(|t| t.text())
                .collect();
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            let joined = lines.join("\n");
            let trimmed = joined.trim();
            if !trimmed.is_empty() {
                blocks.push(trimmed.to_string());
            }
        }
    }
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

impl Builder {
    fn warn(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>, message: impl Into<String>) {
        self.m.warnings.push(SchemaWarning {
            uri: ctx.uri.to_string(),
            pos: Some(ctx.doc.text_pos_at(node.range().start).into()),
            message: message.into(),
        });
    }

    fn process(
        &mut self,
        bundle: &SchemaBundle,
        docs: &[Option<Document<'_>>],
        by_uri: &HashMap<&str, usize>,
        idx: usize,
        tns: String,
    ) {
        if !self.done.insert((idx, tns.clone())) {
            return;
        }
        let Some(doc) = &docs[idx] else { return };
        let uri = bundle.docs[idx].uri.as_str();
        let root = doc.root_element();
        let own_tns = root.attribute("targetNamespace").unwrap_or("");
        let ctx = Ctx {
            uri,
            doc,
            chameleon: own_tns.is_empty() && !tns.is_empty(),
            tns,
            element_qualified: root.attribute("elementFormDefault") == Some("qualified"),
            attribute_qualified: root.attribute("attributeFormDefault") == Some("qualified"),
            block_default: DerivationSet::parse(root.attribute("blockDefault").unwrap_or("")),
            final_default: DerivationSet::parse(root.attribute("finalDefault").unwrap_or("")),
        };
        if !is_xs(root) || root.tag_name().name() != "schema" {
            self.warn(
                &ctx,
                root,
                "root element is not xs:schema; document ignored",
            );
            return;
        }
        for ns in root.namespaces() {
            if let Some(prefix) = ns.name()
                && ns.uri() != XSD_NS
                && self.hinted.insert(ns.uri().to_string())
            {
                self.m
                    .prefix_hints
                    .push((ns.uri().to_string(), prefix.to_string()));
            }
        }
        for child in xs_children(root) {
            match child.tag_name().name() {
                kind @ ("include" | "redefine") => {
                    let target = child
                        .attribute("schemaLocation")
                        .and_then(|l| locate(by_uri, uri, l));
                    let Some(t) = target else {
                        self.warn(
                            &ctx,
                            child,
                            format!("xs:{kind} target is not in the bundle"),
                        );
                        continue;
                    };
                    let t_tns = docs[t]
                        .as_ref()
                        .and_then(|d| d.root_element().attribute("targetNamespace"))
                        .unwrap_or("");
                    if !t_tns.is_empty() && t_tns != ctx.tns {
                        self.warn(
                            &ctx,
                            child,
                            format!("xs:{kind} of a schema for namespace {t_tns}; ignored"),
                        );
                        continue;
                    }
                    self.process(bundle, docs, by_uri, t, ctx.tns.clone());
                    if kind == "redefine" {
                        for r in xs_children(child) {
                            self.global_component(&ctx, r, true);
                        }
                    }
                }
                "import" => {
                    let ns = child.attribute("namespace").unwrap_or("");
                    let found = match child.attribute("schemaLocation") {
                        Some(l) => locate(by_uri, uri, l).is_some(),
                        None => bundle.docs.iter().any(|d| d.target_ns == ns),
                    };
                    if !found && ns != XSD_NS {
                        self.warn(
                            &ctx,
                            child,
                            format!("xs:import of {ns} is not in the bundle"),
                        );
                    }
                }
                _ => self.global_component(&ctx, child, false),
            }
        }
    }

    /// A top-level component. `redefine` replaces an existing definition of the same name.
    fn global_component(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>, redefine: bool) {
        let kind = node.tag_name().name();
        let Some(local) = node.attribute("name") else {
            if !matches!(kind, "notation" | "annotation") {
                self.warn(
                    ctx,
                    node,
                    format!("global xs:{kind} without a name; ignored"),
                );
            }
            return;
        };
        let name = QName::new(ctx.tns.clone(), local.trim());
        match kind {
            "element" => {
                if let Some(id) = self.element(ctx, node, true)
                    && self.m.global_elements.insert(name.clone(), id).is_some()
                {
                    self.warn(ctx, node, format!("duplicate global element {name}"));
                }
            }
            "complexType" | "simpleType" => {
                let old = self.m.global_types.get(&name).copied();
                if old.is_some() && !redefine {
                    self.warn(
                        ctx,
                        node,
                        format!("duplicate global type {name}; first one kept"),
                    );
                    return;
                }
                let id = if kind == "complexType" {
                    self.complex_type(ctx, node, Some(name.clone()), None)
                } else {
                    self.simple_type(ctx, node, Some(name.clone()), None)
                };
                if let Some(old) = old {
                    let td = &mut self.m.types[id.0 as usize];
                    if matches!(&td.base, Some(BaseRef::Named(b)) if *b == name) {
                        td.base = Some(BaseRef::Id(old));
                    }
                }
                self.m.global_types.insert(name, id);
            }
            "group" => {
                let particle = xs_children(node)
                    .find(|c| matches!(c.tag_name().name(), "sequence" | "choice" | "all"))
                    .and_then(|c| self.particle(ctx, c));
                if self.m.groups.contains_key(&name) && !redefine {
                    self.warn(ctx, node, format!("duplicate group {name}; first one kept"));
                } else {
                    self.m.groups.insert(name, GroupDef { particle });
                }
            }
            "attributeGroup" => {
                let mut items = Vec::new();
                let mut any_attribute = None;
                self.attr_items(ctx, node, &mut items, &mut any_attribute);
                if self.m.attr_groups.contains_key(&name) && !redefine {
                    self.warn(ctx, node, format!("duplicate attribute group {name}"));
                } else {
                    self.m.attr_groups.insert(
                        name,
                        AttrGroupDef {
                            items,
                            any_attribute,
                        },
                    );
                }
            }
            "attribute" => {
                if let Some(decl) = self.attribute_decl(ctx, node, true)
                    && self
                        .m
                        .global_attributes
                        .insert(name.clone(), decl)
                        .is_some()
                {
                    self.warn(ctx, node, format!("duplicate global attribute {name}"));
                }
            }
            _ => {}
        }
    }

    /// Resolves a QName-valued attribute against the namespaces in scope at `node`.
    fn qname(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>, value: &str) -> Option<QName> {
        let value = value.trim();
        let (prefix, local) = match value.split_once(':') {
            Some((p, l)) => (Some(p), l),
            None => (None, value),
        };
        let ns = match prefix {
            Some(p) => match node.lookup_namespace_uri(Some(p)) {
                Some(u) => u,
                None => {
                    self.warn(ctx, node, format!("undeclared prefix in {value:?}"));
                    return None;
                }
            },
            None => node.lookup_namespace_uri(None).unwrap_or(""),
        };
        let ns = if ns.is_empty() && ctx.chameleon {
            ctx.tns.as_str()
        } else {
            ns
        };
        Some(QName::new(ns, local))
    }

    fn type_attr(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>, attr: &str) -> Option<TypeRef> {
        let v = node.attribute(attr)?;
        Some(
            self.qname(ctx, node, v)
                .map_or(TypeRef::Default, TypeRef::Named),
        )
    }

    fn element(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>, global: bool) -> Option<ElemId> {
        let Some(local) = node.attribute("name") else {
            self.warn(ctx, node, "xs:element without name or ref; ignored");
            return None;
        };
        let qualified = match node.attribute("form") {
            Some(f) => f == "qualified",
            None => ctx.element_qualified,
        };
        let ns = if global || qualified {
            ctx.tns.as_str()
        } else {
            ""
        };
        let name = QName::new(ns, local.trim());
        let id = ElemId(u32::try_from(self.m.elements.len()).unwrap_or(u32::MAX));
        let mut substitution_heads = Vec::new();
        if let Some(sg) = node.attribute("substitutionGroup") {
            for h in sg.split_ascii_whitespace() {
                if let Some(q) = self.qname(ctx, node, h) {
                    substitution_heads.push(q);
                }
            }
        }
        self.m.elements.push(ElementDecl {
            name: name.clone(),
            type_ref: TypeRef::Default,
            global,
            is_abstract: is_true(node.attribute("abstract")),
            nillable: is_true(node.attribute("nillable")),
            default: node.attribute("default").map(str::to_string),
            fixed: node.attribute("fixed").map(str::to_string),
            substitution_heads,
            block: node
                .attribute("block")
                .map_or(ctx.block_default, DerivationSet::parse),
            final_: node
                .attribute("final")
                .map_or(ctx.final_default, DerivationSet::parse),
            doc: documentation(node),
        });
        let type_ref = match self.type_attr(ctx, node, "type") {
            Some(r) => r,
            None => {
                if let Some(c) = xs_child(node, "complexType") {
                    TypeRef::Anonymous(self.complex_type(ctx, c, None, Some(name)))
                } else if let Some(c) = xs_child(node, "simpleType") {
                    TypeRef::Anonymous(self.simple_type(ctx, c, None, Some(name)))
                } else {
                    TypeRef::Default
                }
            }
        };
        self.m.elements[id.0 as usize].type_ref = type_ref;
        Some(id)
    }

    fn attribute_decl(
        &mut self,
        ctx: &Ctx<'_>,
        node: Node<'_, '_>,
        global: bool,
    ) -> Option<AttributeDecl> {
        let Some(local) = node.attribute("name") else {
            self.warn(ctx, node, "xs:attribute without name or ref; ignored");
            return None;
        };
        let qualified = match node.attribute("form") {
            Some(f) => f == "qualified",
            None => ctx.attribute_qualified,
        };
        let ns = if global || qualified {
            ctx.tns.as_str()
        } else {
            ""
        };
        let name = QName::new(ns, local.trim());
        let type_ref = match self.type_attr(ctx, node, "type") {
            Some(r) => r,
            None => match xs_child(node, "simpleType") {
                Some(c) => TypeRef::Anonymous(self.simple_type(ctx, c, None, Some(name.clone()))),
                None => TypeRef::Default,
            },
        };
        Some(AttributeDecl {
            name,
            type_ref,
            default: node.attribute("default").map(str::to_string),
            fixed: node.attribute("fixed").map(str::to_string),
            doc: documentation(node),
        })
    }

    fn occurs(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>) -> (u32, MaxOccurs) {
        let min = match node.attribute("minOccurs").map(str::trim) {
            None => 1,
            Some(v) => v.parse().unwrap_or_else(|_| {
                self.warn(ctx, node, format!("invalid minOccurs {v:?}"));
                1
            }),
        };
        let max = match node.attribute("maxOccurs").map(str::trim) {
            None => MaxOccurs::Bounded(1),
            Some("unbounded") => MaxOccurs::Unbounded,
            Some(v) => match v.parse() {
                Ok(n) => MaxOccurs::Bounded(n),
                Err(_) => {
                    self.warn(ctx, node, format!("invalid maxOccurs {v:?}"));
                    MaxOccurs::Bounded(1)
                }
            },
        };
        (min, max)
    }

    fn particle(&mut self, ctx: &Ctx<'_>, node: Node<'_, '_>) -> Option<Particle> {
        let (min, max) = self.occurs(ctx, node);
        let term = match node.tag_name().name() {
            "element" => match node.attribute("ref") {
                Some(r) => Term::ElementRef(self.qname(ctx, node, r)?),
                None => Term::Element(self.element(ctx, node, false)?),
            },
            "group" => {
                let Some(r) = node.attribute("ref") else {
                    self.warn(ctx, node, "xs:group without ref; ignored");
                    return None;
                };
                Term::GroupRef(self.qname(ctx, node, r)?)
            }
            kind @ ("sequence" | "choice" | "all") => {
                let children: Vec<Particle> = xs_children(node)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .filter_map(|c| self.particle(ctx, c))
                    .collect();
                match kind {
                    "sequence" => Term::Sequence(children),
                    "choice" => Term::Choice(children),
                    _ => Term::All(children),
                }
            }
            "any" => Term::Any(wildcard(ctx, node)),
            _ => return None,
        };
        Some(Particle { min, max, term })
    }

    fn attr_items(
        &mut self,
        ctx: &Ctx<'_>,
        parent: Node<'_, '_>,
        items: &mut Vec<AttrItem>,
        any: &mut Option<Wildcard>,
    ) {
        for c in xs_children(parent) {
            match c.tag_name().name() {
                "attribute" => {
                    let use_ = match c.attribute("use").map(str::trim) {
                        Some("required") => AttrUse::Required,
                        Some("prohibited") => AttrUse::Prohibited,
                        _ => AttrUse::Optional,
                    };
                    if let Some(r) = c.attribute("ref") {
                        if let Some(name) = self.qname(ctx, c, r) {
                            items.push(AttrItem::Ref {
                                name,
                                use_,
                                default: c.attribute("default").map(str::to_string),
                                fixed: c.attribute("fixed").map(str::to_string),
                            });
                        }
                    } else if let Some(decl) = self.attribute_decl(ctx, c, false) {
                        items.push(AttrItem::Local { decl, use_ });
                    }
                }
                "attributeGroup" => {
                    if let Some(name) = c.attribute("ref").and_then(|r| self.qname(ctx, c, r)) {
                        items.push(AttrItem::Group(name));
                    }
                }
                "anyAttribute" => *any = Some(wildcard(ctx, c)),
                _ => {}
            }
        }
    }

    fn reserve_type(&mut self, name: Option<QName>, context: Option<QName>) -> TypeId {
        let id = TypeId(u32::try_from(self.m.types.len()).unwrap_or(u32::MAX));
        self.m.types.push(TypeDef {
            name,
            context,
            kind: TypeKind::Simple(SimpleVariety::Atomic),
            base: None,
            derivation: None,
            facets: Facets::default(),
            is_abstract: false,
            block: DerivationSet::default(),
            final_: DerivationSet::default(),
            doc: None,
        });
        id
    }

    fn complex_type(
        &mut self,
        ctx: &Ctx<'_>,
        node: Node<'_, '_>,
        name: Option<QName>,
        context: Option<QName>,
    ) -> TypeId {
        let id = self.reserve_type(name, context);
        let mut mixed = is_true(node.attribute("mixed"));
        let mut content = Content::Empty;
        let mut base = None;
        let mut derivation = None;
        let mut attributes = Vec::new();
        let mut any_attribute = None;
        let mut facets = Facets::default();
        for c in xs_children(node) {
            match c.tag_name().name() {
                kind @ ("simpleContent" | "complexContent") => {
                    if c.attribute("mixed").is_some() {
                        mixed = is_true(c.attribute("mixed"));
                    }
                    let Some(d) = xs_children(c)
                        .find(|d| matches!(d.tag_name().name(), "extension" | "restriction"))
                    else {
                        continue;
                    };
                    derivation = Some(if d.tag_name().name() == "extension" {
                        Derivation::Extension
                    } else {
                        Derivation::Restriction
                    });
                    base = d
                        .attribute("base")
                        .and_then(|b| self.qname(ctx, d, b))
                        .map(BaseRef::Named);
                    if kind == "simpleContent" {
                        content = Content::Simple;
                        facets = self.facets(ctx, d);
                    } else if let Some(p) = xs_children(d)
                        .find(|p| {
                            matches!(p.tag_name().name(), "sequence" | "choice" | "all" | "group")
                        })
                        .and_then(|p| self.particle(ctx, p))
                    {
                        content = Content::Elements(p);
                    }
                    self.attr_items(ctx, d, &mut attributes, &mut any_attribute);
                }
                "sequence" | "choice" | "all" | "group" => {
                    if let Some(p) = self.particle(ctx, c) {
                        content = Content::Elements(p);
                    }
                }
                _ => {}
            }
        }
        self.attr_items(ctx, node, &mut attributes, &mut any_attribute);
        let td = &mut self.m.types[id.0 as usize];
        td.kind = TypeKind::Complex {
            content,
            mixed,
            attributes,
            any_attribute,
        };
        td.base = base;
        td.derivation = derivation;
        td.facets = facets;
        td.is_abstract = is_true(node.attribute("abstract"));
        td.block = node
            .attribute("block")
            .map_or(ctx.block_default, DerivationSet::parse);
        td.final_ = node
            .attribute("final")
            .map_or(ctx.final_default, DerivationSet::parse);
        td.doc = documentation(node);
        id
    }

    fn simple_type(
        &mut self,
        ctx: &Ctx<'_>,
        node: Node<'_, '_>,
        name: Option<QName>,
        context: Option<QName>,
    ) -> TypeId {
        let id = self.reserve_type(name, context.clone());
        let mut variety = SimpleVariety::Atomic;
        let mut base = None;
        let mut derivation = None;
        let mut facets = Facets::default();
        for c in xs_children(node) {
            match c.tag_name().name() {
                "restriction" => {
                    derivation = Some(Derivation::Restriction);
                    base = match c.attribute("base") {
                        Some(b) => self.qname(ctx, c, b).map(BaseRef::Named),
                        None => xs_child(c, "simpleType")
                            .map(|s| BaseRef::Id(self.simple_type(ctx, s, None, context.clone()))),
                    };
                    facets = self.facets(ctx, c);
                }
                "list" => {
                    derivation = Some(Derivation::List);
                    let item = match self.type_attr(ctx, c, "itemType") {
                        Some(r) => r,
                        None => match xs_child(c, "simpleType") {
                            Some(s) => {
                                TypeRef::Anonymous(self.simple_type(ctx, s, None, context.clone()))
                            }
                            None => TypeRef::Default,
                        },
                    };
                    variety = SimpleVariety::List(item);
                }
                "union" => {
                    derivation = Some(Derivation::Union);
                    let mut members = Vec::new();
                    for m in c
                        .attribute("memberTypes")
                        .unwrap_or("")
                        .split_ascii_whitespace()
                    {
                        if let Some(q) = self.qname(ctx, c, m) {
                            members.push(TypeRef::Named(q));
                        }
                    }
                    for s in xs_children(c).filter(|s| s.tag_name().name() == "simpleType") {
                        members.push(TypeRef::Anonymous(self.simple_type(
                            ctx,
                            s,
                            None,
                            context.clone(),
                        )));
                    }
                    variety = SimpleVariety::Union(members);
                }
                _ => {}
            }
        }
        let td = &mut self.m.types[id.0 as usize];
        td.kind = TypeKind::Simple(variety);
        td.base = base;
        td.derivation = derivation;
        td.facets = facets;
        td.final_ = node
            .attribute("final")
            .map_or(ctx.final_default, DerivationSet::parse);
        td.doc = documentation(node);
        id
    }

    fn facets(&mut self, _ctx: &Ctx<'_>, restriction: Node<'_, '_>) -> Facets {
        let mut f = Facets::default();
        for c in xs_children(restriction) {
            let Some(v) = c.attribute("value") else {
                continue;
            };
            let v = v.to_string();
            match c.tag_name().name() {
                "enumeration" => f.enumerations.push((v, documentation(c))),
                "pattern" => f.patterns.push(v),
                "minInclusive" => f.min_inclusive = Some(v),
                "maxInclusive" => f.max_inclusive = Some(v),
                "minExclusive" => f.min_exclusive = Some(v),
                "maxExclusive" => f.max_exclusive = Some(v),
                "length" => f.length = Some(v),
                "minLength" => f.min_length = Some(v),
                "maxLength" => f.max_length = Some(v),
                "totalDigits" => f.total_digits = Some(v),
                "fractionDigits" => f.fraction_digits = Some(v),
                _ => {}
            }
        }
        f
    }

    /// Builds the derivation and substitution indexes and reports dangling references.
    fn finish(mut self) -> SchemaModel {
        let m = &mut self.m;
        let mut derived: HashMap<TypeKey, Vec<TypeId>> = HashMap::new();
        for i in 0..m.types.len() {
            let id = TypeId(u32::try_from(i).unwrap_or(u32::MAX));
            if let Some(k) = m.base_key(id) {
                derived.entry(k).or_default().push(id);
            }
        }
        m.derived = derived;

        let mut substitutes: HashMap<ElemId, Vec<ElemId>> = HashMap::new();
        let mut by_ns: HashMap<String, Vec<ElemId>> = HashMap::new();
        let mut dangling: BTreeSet<String> = BTreeSet::new();
        for (i, e) in m.elements.iter().enumerate() {
            let id = ElemId(u32::try_from(i).unwrap_or(u32::MAX));
            if let TypeRef::Named(q) = &e.type_ref
                && m.resolve_type(q) == TypeKey::Unknown
            {
                dangling.insert(format!("type {q}"));
            }
            if !e.global || m.global_elements.get(&e.name) != Some(&id) {
                continue;
            }
            by_ns.entry(e.name.ns.clone()).or_default().push(id);
            for h in &e.substitution_heads {
                match m.global_elements.get(h) {
                    Some(&head) => substitutes.entry(head).or_default().push(id),
                    None => {
                        dangling.insert(format!("substitution group head {h}"));
                    }
                }
            }
        }
        m.substitutes = substitutes;
        m.elements_by_ns = by_ns;

        let mut attrs_by_ns: HashMap<String, Vec<QName>> = HashMap::new();
        for q in m.global_attributes.keys() {
            attrs_by_ns.entry(q.ns.clone()).or_default().push(q.clone());
        }
        for v in attrs_by_ns.values_mut() {
            v.sort();
        }
        m.attributes_by_ns = attrs_by_ns;

        for td in &m.types {
            if let Some(BaseRef::Named(q)) = &td.base
                && m.resolve_type(q) == TypeKey::Unknown
            {
                dangling.insert(format!("type {q}"));
            }
            if let TypeKind::Complex {
                content: Content::Elements(p),
                ..
            } = &td.kind
            {
                check_particle(m, p, &mut dangling);
            }
        }
        for g in m.groups.values() {
            if let Some(p) = &g.particle {
                check_particle(m, p, &mut dangling);
            }
        }
        for d in dangling {
            m.warnings.push(SchemaWarning {
                uri: String::new(),
                pos: None,
                message: format!("unresolved reference to {d}"),
            });
        }
        self.m
    }
}

fn check_particle(m: &SchemaModel, p: &Particle, dangling: &mut BTreeSet<String>) {
    match &p.term {
        Term::ElementRef(q) if !m.global_elements.contains_key(q) => {
            dangling.insert(format!("element {q}"));
        }
        Term::GroupRef(q) if !m.groups.contains_key(q) => {
            dangling.insert(format!("group {q}"));
        }
        Term::Sequence(c) | Term::Choice(c) | Term::All(c) => {
            for p in c {
                check_particle(m, p, dangling);
            }
        }
        _ => {}
    }
}

fn wildcard(ctx: &Ctx<'_>, node: Node<'_, '_>) -> Wildcard {
    let ns = node.attribute("namespace").unwrap_or("##any").trim();
    let namespaces = match ns {
        "##any" => NamespaceConstraint::Any,
        "##other" => NamespaceConstraint::Not(ctx.tns.clone()),
        list => NamespaceConstraint::List(
            list.split_ascii_whitespace()
                .map(|t| match t {
                    "##targetNamespace" => ctx.tns.clone(),
                    "##local" => String::new(),
                    other => other.to_string(),
                })
                .collect(),
        ),
    };
    let process_contents = match node.attribute("processContents").map(str::trim) {
        Some("lax") => ProcessContents::Lax,
        Some("skip") => ProcessContents::Skip,
        _ => ProcessContents::Strict,
    };
    Wildcard {
        namespaces,
        process_contents,
    }
}
