//! Pure-Rust XSD model used for completion, hover and template generation (not validation).
//!
//! Owned by WP-SCHEMA (`docs/TASKS.md`). Details: `docs/PLAN.md` §5, §5.1, §5.2.
//!
//! libxml2 is authoritative for validation; this model only has to be good enough to suggest
//! things. Where it is incomplete (exotic XSD corners) the worst case is a missing or extra
//! suggestion, never a wrong validation result. Consequently building never fails: malformed
//! documents and dangling references become [`SchemaWarning`]s and the rest of the bundle is
//! still indexed.
//!
//! The model is built once per schema compile ([`SchemaModel::build`]) and is immutable
//! afterwards, so it can be shared across threads behind an `Arc`.
//!
//! Supported: global and local elements/attributes (with `form` and the `*FormDefault`s),
//! named and anonymous complex/simple types, `sequence`/`choice`/`all`, named groups and
//! attribute groups, `simpleContent`/`complexContent` extension and restriction, `list` and
//! `union`, facets, `abstract`/`block`/`final` (+ defaults), substitution groups, `xs:any` and
//! `xs:anyAttribute`, `xs:include` (incl. chameleon), `xs:import`, and `xs:redefine` of types.
//! Not modelled: identity constraints, `xs:redefine` of groups and attribute groups, XSD 1.1.

mod build;
mod builtin;
mod component;
mod query;
mod template;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::diag::TextPos;
use crate::model::QName;
use crate::soap::XSD_NS;

use builtin::{ANY_SIMPLE_TYPE, ANY_TYPE, Builtin};
use component::{
    AttrGroupDef, AttrItem, AttrUse, AttributeDecl, BaseRef, Content, ElemId, ElementDecl,
    GroupDef, Particle, SimpleVariety, TypeDef, TypeId, TypeKind, TypeRef,
};

pub use component::{
    Derivation, DerivationSet, MaxOccurs, NamespaceConstraint, ProcessContents, Wildcard,
};
pub use query::{
    AttributeCompletions, AttributeSuggestion, ChildCompletions, ChildElement, ElementInfo,
    EnumValue, PathStep, SuggestionSource, TypeInfo, TypeSuggestion,
};
pub use template::{Template, TemplateOptions};

/// Guard for walks along base chains, group references and substitution heads. Real schemas
/// stay far below this; it only stops cycles in broken ones.
const MAX_CHAIN: usize = 64;

/// A problem found while building the model. Not fatal: the affected component is skipped or
/// treated as `xs:anyType`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaWarning {
    /// [`crate::model::SchemaDoc::uri`] of the document, empty if not tied to one.
    pub uri: String,
    pub pos: Option<TextPos>,
    pub message: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("no global element {0} in the schema")]
    UnknownElement(QName),
}

/// What a type reference resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TypeKey {
    Def(TypeId),
    Builtin(Builtin),
    /// A dangling reference; behaves like `xs:anyType` without suggestions.
    Unknown,
}

/// An attribute in a type's effective attribute set.
#[derive(Debug, Clone)]
pub(crate) struct EffAttr {
    pub name: QName,
    pub type_key: TypeKey,
    pub type_name: Option<QName>,
    pub required: bool,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub doc: Option<String>,
}

/// The indexed schema model of a [`crate::model::SchemaBundle`].
#[derive(Debug, Default)]
pub struct SchemaModel {
    pub(crate) elements: Vec<ElementDecl>,
    pub(crate) types: Vec<TypeDef>,
    pub(crate) global_elements: HashMap<QName, ElemId>,
    pub(crate) global_types: HashMap<QName, TypeId>,
    pub(crate) groups: HashMap<QName, GroupDef>,
    pub(crate) attr_groups: HashMap<QName, AttrGroupDef>,
    pub(crate) global_attributes: HashMap<QName, AttributeDecl>,
    /// Direct derivations, base → derived types in definition order.
    pub(crate) derived: HashMap<TypeKey, Vec<TypeId>>,
    /// Direct substitution group members per head, in definition order.
    pub(crate) substitutes: HashMap<ElemId, Vec<ElemId>>,
    /// Global elements per namespace, in definition order (wildcard expansion).
    pub(crate) elements_by_ns: HashMap<String, Vec<ElemId>>,
    /// Global attributes per namespace (attribute wildcard expansion).
    pub(crate) attributes_by_ns: HashMap<String, Vec<QName>>,
    /// `(namespace, prefix)` as declared on the schema documents, first declaration wins.
    pub(crate) prefix_hints: Vec<(String, String)>,
    pub(crate) warnings: Vec<SchemaWarning>,
}

impl SchemaModel {
    /// Problems found while building; shown in the import report, never fatal.
    pub fn warnings(&self) -> &[SchemaWarning] {
        &self.warnings
    }

    /// Non-abstract global elements in definition order.
    pub fn global_elements(&self) -> Vec<QName> {
        let mut ids: Vec<ElemId> = self.global_elements.values().copied().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| self.elem(id))
            .filter(|e| !e.is_abstract)
            .map(|e| e.name.clone())
            .collect()
    }

    /// Prefixes used for these namespaces in the schema documents; templates reuse them.
    pub fn prefix_hints(&self) -> &[(String, String)] {
        &self.prefix_hints
    }

    pub(crate) fn elem(&self, id: ElemId) -> &ElementDecl {
        &self.elements[id.0 as usize]
    }

    pub(crate) fn ty(&self, id: TypeId) -> &TypeDef {
        &self.types[id.0 as usize]
    }

    pub(crate) fn resolve_type(&self, name: &QName) -> TypeKey {
        if let Some(&id) = self.global_types.get(name) {
            return TypeKey::Def(id);
        }
        if name.ns == XSD_NS
            && let Some(b) = Builtin::by_name(&name.local)
        {
            return TypeKey::Builtin(b);
        }
        TypeKey::Unknown
    }

    fn type_ref_key(&self, r: &TypeRef, default: Builtin) -> TypeKey {
        match r {
            TypeRef::Named(q) => self.resolve_type(q),
            TypeRef::Anonymous(id) => TypeKey::Def(*id),
            TypeRef::Default => TypeKey::Builtin(default),
        }
    }

    /// The type of an element declaration. Substitution group members without their own type
    /// take the head's type.
    pub(crate) fn elem_type(&self, id: ElemId) -> TypeKey {
        let mut cur = id;
        for _ in 0..MAX_CHAIN {
            let e = self.elem(cur);
            match &e.type_ref {
                TypeRef::Default => match e
                    .substitution_heads
                    .first()
                    .and_then(|h| self.global_elements.get(h))
                {
                    Some(&head) => cur = head,
                    None => return TypeKey::Builtin(ANY_TYPE),
                },
                r => return self.type_ref_key(r, ANY_TYPE),
            }
        }
        TypeKey::Unknown
    }

    pub(crate) fn base_key(&self, id: TypeId) -> Option<TypeKey> {
        match self.ty(id).base.as_ref()? {
            BaseRef::Named(q) => Some(self.resolve_type(q)),
            BaseRef::Id(b) => Some(TypeKey::Def(*b)),
        }
    }

    pub(crate) fn type_name(&self, key: TypeKey) -> Option<QName> {
        match key {
            TypeKey::Def(id) => self.ty(id).name.clone(),
            TypeKey::Builtin(b) => Some(QName::new(XSD_NS, b.info().name)),
            TypeKey::Unknown => None,
        }
    }

    pub(crate) fn type_doc(&self, key: TypeKey) -> Option<&str> {
        match key {
            TypeKey::Def(id) => self.ty(id).doc.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn is_abstract_type(&self, key: TypeKey) -> bool {
        matches!(key, TypeKey::Def(id) if self.ty(id).is_abstract)
    }

    /// The derivation methods on the way from `from` up to `to`, or `None` if `from` is not
    /// derived from `to` (a type counts as derived from itself, with no methods).
    pub(crate) fn derivation_methods(&self, from: TypeKey, to: TypeKey) -> Option<Vec<Derivation>> {
        let mut methods = Vec::new();
        let mut cur = from;
        for _ in 0..MAX_CHAIN {
            if cur == to {
                return Some(methods);
            }
            match cur {
                TypeKey::Def(id) => {
                    let td = self.ty(id);
                    methods.push(td.derivation.unwrap_or(Derivation::Restriction));
                    cur = match self.base_key(id) {
                        Some(k) => k,
                        // No base: implicit restriction of anyType / anySimpleType.
                        None if td.is_complex() => TypeKey::Builtin(ANY_TYPE),
                        None => TypeKey::Builtin(ANY_SIMPLE_TYPE),
                    };
                }
                TypeKey::Builtin(b) => {
                    methods.push(Derivation::Restriction);
                    cur = TypeKey::Builtin(b.base()?);
                }
                TypeKey::Unknown => return None,
            }
        }
        None
    }

    /// All types transitively derived from `base`, in definition order. A derivation whose
    /// method is in `blocked` cuts off that type and everything derived from it.
    pub(crate) fn derived_types(&self, base: TypeKey, blocked: DerivationSet) -> Vec<TypeId> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut todo = vec![base];
        while let Some(k) = todo.pop() {
            for &d in self.derived.get(&k).map(Vec::as_slice).unwrap_or_default() {
                let method = self.ty(d).derivation.unwrap_or(Derivation::Restriction);
                if blocked.blocks(method) || !seen.insert(d) {
                    continue;
                }
                out.push(d);
                todo.push(TypeKey::Def(d));
            }
        }
        out.sort();
        out
    }

    /// Concrete types usable as `xsi:type` for an element declared with `declared`, honouring
    /// the element's and the declared type's `block`. The declared type itself is included
    /// first when it is not abstract.
    pub(crate) fn xsi_type_options(
        &self,
        declared: TypeKey,
        elem_block: DerivationSet,
    ) -> Vec<TypeKey> {
        let mut blocked = elem_block;
        if let TypeKey::Def(id) = declared {
            blocked = blocked.union_with(self.ty(id).block);
        }
        let mut out = Vec::new();
        if !self.is_abstract_type(declared) && declared != TypeKey::Unknown {
            out.push(declared);
        }
        out.extend(
            self.derived_types(declared, blocked)
                .into_iter()
                .filter(|&d| !self.ty(d).is_abstract)
                .map(TypeKey::Def),
        );
        out
    }

    /// Transitive substitution group members of `head` that may actually substitute for it:
    /// not blocked by the head's `block`, in definition order. Abstract members are included;
    /// callers filter them for suggestions.
    pub(crate) fn substitution_members(&self, head: ElemId) -> Vec<ElemId> {
        let h = self.elem(head);
        if h.block.substitution {
            return Vec::new();
        }
        let head_type = self.elem_type(head);
        let mut out = Vec::new();
        let mut seen = HashSet::from([head]);
        let mut todo = vec![head];
        while let Some(cur) = todo.pop() {
            for &m in self
                .substitutes
                .get(&cur)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                if !seen.insert(m) {
                    continue;
                }
                todo.push(m);
                let methods = self.derivation_methods(self.elem_type(m), head_type);
                let allowed = match methods {
                    Some(ms) => !ms.iter().any(|&d| h.block.blocks(d) || h.final_.blocks(d)),
                    // Not validly derived: libxml2 rejects the schema; still suggest it.
                    None => true,
                };
                if allowed {
                    out.push(m);
                }
            }
        }
        out.sort();
        out
    }

    /// The particles making up a type's element content, base type first for extensions.
    pub(crate) fn content_particles(&self, key: TypeKey) -> Vec<&Particle> {
        let mut chain = Vec::new();
        let mut cur = key;
        for _ in 0..MAX_CHAIN {
            let TypeKey::Def(id) = cur else { break };
            let td = self.ty(id);
            let TypeKind::Complex { content, .. } = &td.kind else {
                break;
            };
            if let Content::Elements(p) = content {
                chain.push(p);
            }
            if td.derivation != Some(Derivation::Extension) {
                break;
            }
            match self.base_key(id) {
                Some(k) => cur = k,
                None => break,
            }
        }
        chain.reverse();
        chain
    }

    /// Whether elements of this type carry text (simple type or simple content).
    pub(crate) fn has_simple_value(&self, key: TypeKey) -> bool {
        match key {
            TypeKey::Def(id) => match &self.ty(id).kind {
                TypeKind::Simple(_) => true,
                TypeKind::Complex { content, .. } => matches!(content, Content::Simple),
            },
            TypeKey::Builtin(b) => !b.is_any_type(),
            TypeKey::Unknown => false,
        }
    }

    pub(crate) fn is_mixed(&self, key: TypeKey) -> bool {
        matches!(key, TypeKey::Def(id)
            if matches!(self.ty(id).kind, TypeKind::Complex { mixed: true, .. }))
    }

    /// Enumeration values of a simple type (or simple content): those of the most derived
    /// restriction that has any, collected across union members. `xs:boolean` offers
    /// `true`/`false`.
    pub(crate) fn enumerations(&self, key: TypeKey) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        self.collect_enumerations(key, &mut out, 0);
        out
    }

    fn collect_enumerations(
        &self,
        key: TypeKey,
        out: &mut Vec<(String, Option<String>)>,
        depth: usize,
    ) {
        let mut cur = key;
        for _ in depth..MAX_CHAIN {
            match cur {
                TypeKey::Def(id) => {
                    let td = self.ty(id);
                    if !td.facets.enumerations.is_empty() {
                        out.extend(td.facets.enumerations.iter().cloned());
                        return;
                    }
                    match &td.kind {
                        TypeKind::Simple(SimpleVariety::Union(members)) => {
                            for m in members {
                                let mk = self.type_ref_key(m, ANY_SIMPLE_TYPE);
                                self.collect_enumerations(mk, out, depth + 8);
                            }
                            return;
                        }
                        TypeKind::Simple(SimpleVariety::List(item)) => {
                            cur = self.type_ref_key(item, ANY_SIMPLE_TYPE);
                            continue;
                        }
                        _ => {}
                    }
                    match self.base_key(id) {
                        Some(k) => cur = k,
                        None => return,
                    }
                }
                TypeKey::Builtin(b) => {
                    if b.is_boolean() {
                        out.push(("true".into(), None));
                        out.push(("false".into(), None));
                    }
                    return;
                }
                TypeKey::Unknown => return,
            }
        }
    }

    /// The effective attribute uses of a type (inherited, groups expanded, prohibited removed)
    /// and its attribute wildcard.
    pub(crate) fn effective_attributes(&self, key: TypeKey) -> (Vec<EffAttr>, Option<Wildcard>) {
        let mut chain = Vec::new();
        let mut cur = key;
        for _ in 0..MAX_CHAIN {
            let TypeKey::Def(id) = cur else { break };
            if !self.ty(id).is_complex() || chain.contains(&id) {
                break;
            }
            chain.push(id);
            match self.base_key(id) {
                Some(k) => cur = k,
                None => break,
            }
        }
        let mut attrs: Vec<EffAttr> = Vec::new();
        let mut wildcard = None;
        for &id in chain.iter().rev() {
            let td = self.ty(id);
            let TypeKind::Complex {
                attributes,
                any_attribute,
                ..
            } = &td.kind
            else {
                continue;
            };
            let mut own_wildcard = any_attribute.clone();
            self.apply_attr_items(attributes, &mut attrs, &mut own_wildcard, 0);
            if own_wildcard.is_some() || td.derivation == Some(Derivation::Restriction) {
                wildcard = own_wildcard;
            }
        }
        (attrs, wildcard)
    }

    fn apply_attr_items(
        &self,
        items: &[AttrItem],
        attrs: &mut Vec<EffAttr>,
        wildcard: &mut Option<Wildcard>,
        depth: usize,
    ) {
        if depth > MAX_CHAIN {
            return;
        }
        for item in items {
            let (eff, use_) = match item {
                AttrItem::Local { decl, use_ } => (self.eff_attr(decl, None, None), *use_),
                AttrItem::Ref {
                    name,
                    use_,
                    default,
                    fixed,
                } => match self.global_attributes.get(name) {
                    Some(decl) => (self.eff_attr(decl, default.clone(), fixed.clone()), *use_),
                    None => continue,
                },
                AttrItem::Group(name) => {
                    if let Some(g) = self.attr_groups.get(name) {
                        self.apply_attr_items(&g.items, attrs, wildcard, depth + 1);
                        if wildcard.is_none() {
                            wildcard.clone_from(&g.any_attribute);
                        }
                    }
                    continue;
                }
            };
            attrs.retain(|a| a.name != eff.name);
            if use_ != AttrUse::Prohibited {
                attrs.push(EffAttr {
                    required: use_ == AttrUse::Required,
                    ..eff
                });
            }
        }
    }

    fn eff_attr(
        &self,
        decl: &AttributeDecl,
        default: Option<String>,
        fixed: Option<String>,
    ) -> EffAttr {
        let type_key = self.type_ref_key(&decl.type_ref, ANY_SIMPLE_TYPE);
        EffAttr {
            name: decl.name.clone(),
            type_key,
            type_name: self.type_name(type_key),
            required: false,
            default: default.or_else(|| decl.default.clone()),
            fixed: fixed.or_else(|| decl.fixed.clone()),
            doc: decl.doc.clone(),
        }
    }

    /// The template placeholder for a simple value of this type.
    pub(crate) fn placeholder(&self, key: TypeKey) -> String {
        if let Some((first, _)) = self.enumerations(key).into_iter().next() {
            return first;
        }
        let mut min_inclusive = None;
        let mut cur = key;
        for _ in 0..MAX_CHAIN {
            match cur {
                TypeKey::Def(id) => {
                    let td = self.ty(id);
                    if min_inclusive.is_none() {
                        min_inclusive = td.facets.min_inclusive.clone();
                    }
                    cur = match &td.kind {
                        TypeKind::Simple(SimpleVariety::List(item)) => {
                            self.type_ref_key(item, ANY_SIMPLE_TYPE)
                        }
                        TypeKind::Simple(SimpleVariety::Union(m)) => match m.first() {
                            Some(r) => self.type_ref_key(r, ANY_SIMPLE_TYPE),
                            None => return "?".into(),
                        },
                        _ => match self.base_key(id) {
                            Some(k) => k,
                            None => return "?".into(),
                        },
                    };
                }
                TypeKey::Builtin(b) => {
                    let info = b.info();
                    return match min_inclusive {
                        Some(v) if info.numeric => v,
                        _ => info.placeholder.to_string(),
                    };
                }
                TypeKey::Unknown => return "?".into(),
            }
        }
        "?".into()
    }

    /// A display name for a type: its QName, or "anonymous type of `x`".
    pub(crate) fn type_label(
        &self,
        key: TypeKey,
        fmt_q: &mut dyn FnMut(&QName) -> String,
    ) -> String {
        match key {
            TypeKey::Def(id) => {
                let td = self.ty(id);
                match (&td.name, &td.context) {
                    (Some(n), _) => fmt_q(n),
                    (None, Some(c)) => format!("anonymous type of {}", fmt_q(c)),
                    (None, None) => "anonymous type".into(),
                }
            }
            TypeKey::Builtin(b) => format!("xs:{}", b.info().name),
            TypeKey::Unknown => "unknown type".into(),
        }
    }
}
