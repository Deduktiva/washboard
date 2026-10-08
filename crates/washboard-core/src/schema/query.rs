//! Completion and hover queries.
//!
//! Every query takes the element path from the root block element (a `Body` or `Header`
//! child, which must be a global element) down to the element the cursor is in, as resolved
//! QNames. Resolving prefixes is the caller's job (the editor's cursor context, WP-XML), so
//! the model never sees document text. An unresolvable path yields empty results, never an
//! error: the user may be typing something the schema does not allow.

use std::collections::HashMap;

use crate::model::QName;
use crate::soap::{SOAP11_ENV_NS, XSI_NS};
use crate::xml;

use super::builtin::ANY_TYPE;
use super::component::{ElemId, MaxOccurs, Particle, ProcessContents, Term, TypeKind, Wildcard};
use super::{Derivation, MAX_CHAIN, SchemaModel, TypeKey};

/// One element of the path from the root block element to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub name: QName,
    /// The resolved `xsi:type` written on this element, if any. It replaces the declared
    /// type for everything below (PLAN §5.2).
    pub xsi_type: Option<QName>,
}

impl PathStep {
    pub fn new(name: QName) -> Self {
        Self {
            name,
            xsi_type: None,
        }
    }

    pub fn with_xsi_type(mut self, xsi_type: QName) -> Self {
        self.xsi_type = Some(xsi_type);
        self
    }
}

/// The schema path for an editor path (outermost first), as the queries here take it: from the element inside a SOAP
/// `Header` or `Body` down to the last one. `None` outside header and body blocks, or if a
/// name on the way does not resolve.
pub fn block_path(path: &[xml::PathElement]) -> Option<Vec<PathStep>> {
    let is_block_parent = |e: &xml::PathElement| {
        e.name
            .as_ref()
            .is_some_and(|n| n.ns == SOAP11_ENV_NS && matches!(n.local.as_str(), "Body" | "Header"))
    };
    let first = path.iter().position(is_block_parent)? + 1;
    path.get(first..)
        .filter(|block| !block.is_empty())?
        .iter()
        .map(|e| {
            let step = PathStep::new(e.name.clone()?);
            Some(match e.xsi_type.as_ref().and_then(|t| t.name.clone()) {
                Some(t) => step.with_xsi_type(t),
                None => step,
            })
        })
        .collect()
}

/// Why a name is offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionSource {
    /// Declared in the content model (or attribute set) of the type.
    Declared,
    /// A substitution group member standing in for `head` (which may be abstract).
    Substitution { head: QName },
    /// A global declaration matched by `xs:any` / `xs:anyAttribute`.
    Wildcard,
    /// `xsi:type` or `xsi:nil`.
    Xsi,
}

/// An element allowed as a child at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildElement {
    pub name: QName,
    /// Effective cardinality at this position, including enclosing groups (a branch of a
    /// multi-branch choice has `min_occurs` 0).
    pub min_occurs: u32,
    pub max_occurs: MaxOccurs,
    /// `None` for anonymous types.
    pub type_name: Option<QName>,
    pub source: SuggestionSource,
    /// The type is abstract: an instance needs `xsi:type`.
    pub type_is_abstract: bool,
    /// Concrete types derived from the declared type exist (`xsi:type` is meaningful).
    pub has_derived_types: bool,
    pub documentation: Option<String>,
}

/// Result of [`SchemaModel::child_elements`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChildCompletions {
    /// In content-model order, each name once.
    pub elements: Vec<ChildElement>,
    /// The wildcards at this position. For `processContents="skip"` they are the only hint;
    /// otherwise the matching global elements are already in `elements`.
    pub wildcards: Vec<Wildcard>,
    /// Text is allowed between the children.
    pub mixed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeSuggestion {
    pub name: QName,
    pub required: bool,
    pub type_name: Option<QName>,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub source: SuggestionSource,
    pub documentation: Option<String>,
}

/// Result of [`SchemaModel::attributes`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttributeCompletions {
    pub attributes: Vec<AttributeSuggestion>,
    /// The attribute wildcard; for non-`skip` ones the matching global attributes are in
    /// `attributes`.
    pub wildcard: Option<Wildcard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    pub value: String,
    pub documentation: Option<String>,
}

/// A candidate `xsi:type` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSuggestion {
    pub name: QName,
    /// This is the element's declared type itself (writing it is allowed but redundant).
    pub is_declared: bool,
    pub documentation: Option<String>,
}

/// Hover information for the element at the end of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementInfo {
    pub name: QName,
    /// `None` for anonymous types or an element matched by a `skip` wildcard.
    pub declared_type: Option<QName>,
    /// Differs from `declared_type` when `xsi:type` is set ("declared `com:Party`, actual
    /// `com:Company` via xsi:type").
    pub actual_type: Option<QName>,
    pub min_occurs: u32,
    pub max_occurs: MaxOccurs,
    pub is_abstract: bool,
    pub type_is_abstract: bool,
    pub nillable: bool,
    /// The element's documentation, falling back to its type's.
    pub documentation: Option<String>,
}

/// Hover information for a named type (e.g. an `xsi:type` value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeInfo {
    pub name: QName,
    pub base: Option<QName>,
    pub derivation: Option<Derivation>,
    pub is_abstract: bool,
    pub is_complex: bool,
    pub enumerations: Vec<EnumValue>,
    pub patterns: Vec<String>,
    pub documentation: Option<String>,
}

/// A path step resolved against the model.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resolved {
    /// `None` when matched by a wildcard without a known global declaration.
    pub elem: Option<ElemId>,
    pub declared: TypeKey,
    pub effective: TypeKey,
    pub min: u32,
    pub max: MaxOccurs,
}

/// A position in a flattened content model.
#[derive(Debug)]
pub(crate) enum Slot<'a> {
    Elem {
        id: ElemId,
        min: u32,
        max: MaxOccurs,
    },
    Any {
        wildcard: &'a Wildcard,
        min: u32,
        max: MaxOccurs,
    },
}

impl SchemaModel {
    /// The content model of a type flattened to element declarations and wildcards with
    /// their effective cardinality, in document order.
    pub(crate) fn slots(&self, key: TypeKey) -> Vec<Slot<'_>> {
        let mut out = Vec::new();
        for p in self.content_particles(key) {
            self.walk(p, 1, MaxOccurs::Bounded(1), &mut out, 0);
        }
        out
    }

    fn walk<'a>(
        &'a self,
        p: &'a Particle,
        min_mul: u32,
        max_mul: MaxOccurs,
        out: &mut Vec<Slot<'a>>,
        depth: usize,
    ) {
        if p.max.is_zero() || depth > MAX_CHAIN {
            return;
        }
        let min = min_mul.saturating_mul(p.min);
        let max = max_mul.mul(p.max);
        match &p.term {
            Term::Element(id) => out.push(Slot::Elem { id: *id, min, max }),
            Term::ElementRef(q) => {
                if let Some(&id) = self.global_elements.get(q) {
                    out.push(Slot::Elem { id, min, max });
                }
            }
            Term::GroupRef(q) => {
                if let Some(gp) = self.groups.get(q).and_then(|g| g.particle.as_ref()) {
                    self.walk(gp, min, max, out, depth + 1);
                }
            }
            Term::Sequence(c) | Term::All(c) => {
                for cp in c {
                    self.walk(cp, min, max, out, depth + 1);
                }
            }
            Term::Choice(c) => {
                let branch_min = if c.len() > 1 { 0 } else { min };
                for cp in c {
                    self.walk(cp, branch_min, max, out, depth + 1);
                }
            }
            Term::Any(w) => out.push(Slot::Any {
                wildcard: w,
                min,
                max,
            }),
        }
    }

    fn apply_xsi(&self, declared: TypeKey, step: &PathStep) -> TypeKey {
        match step.xsi_type.as_ref().map(|q| self.resolve_type(q)) {
            Some(k) if k != TypeKey::Unknown => k,
            _ => declared,
        }
    }

    fn resolved_elem(&self, id: ElemId, step: &PathStep, min: u32, max: MaxOccurs) -> Resolved {
        let declared = self.elem_type(id);
        Resolved {
            elem: Some(id),
            declared,
            effective: self.apply_xsi(declared, step),
            min,
            max,
        }
    }

    pub(crate) fn resolve(&self, path: &[PathStep]) -> Option<Resolved> {
        let (first, rest) = path.split_first()?;
        let &root = self.global_elements.get(&first.name)?;
        let mut cur = self.resolved_elem(root, first, 1, MaxOccurs::Bounded(1));
        for step in rest {
            cur = self.child_of(&cur, step)?;
        }
        Some(cur)
    }

    fn child_of(&self, parent: &Resolved, step: &PathStep) -> Option<Resolved> {
        let slots = self.slots(parent.effective);
        for s in &slots {
            let Slot::Elem { id, min, max } = *s else {
                continue;
            };
            let e = self.elem(id);
            if e.name == step.name {
                return Some(self.resolved_elem(id, step, min, max));
            }
            if e.global
                && let Some(m) = self
                    .substitution_members(id)
                    .into_iter()
                    .find(|&m| self.elem(m).name == step.name)
            {
                return Some(self.resolved_elem(m, step, min, max));
            }
        }
        for s in &slots {
            let Slot::Any { wildcard, min, max } = *s else {
                continue;
            };
            if !wildcard.namespaces.allows(&step.name.ns) {
                continue;
            }
            let known = self.global_elements.get(&step.name).copied();
            return Some(match (wildcard.process_contents, known) {
                (ProcessContents::Skip, _) | (_, None) => Resolved {
                    elem: None,
                    declared: TypeKey::Unknown,
                    effective: self.apply_xsi(TypeKey::Unknown, step),
                    min,
                    max,
                },
                (_, Some(id)) => self.resolved_elem(id, step, min, max),
            });
        }
        // xs:anyType content is a lax ##any wildcard.
        if parent.effective == TypeKey::Builtin(ANY_TYPE) {
            let id = self.global_elements.get(&step.name).copied()?;
            return Some(self.resolved_elem(id, step, 0, MaxOccurs::Unbounded));
        }
        None
    }

    fn child_element(
        &self,
        id: ElemId,
        min: u32,
        max: MaxOccurs,
        source: SuggestionSource,
    ) -> ChildElement {
        let e = self.elem(id);
        let ty = self.elem_type(id);
        let has_derived = self.xsi_type_options(ty, e.block).iter().any(|&k| k != ty);
        ChildElement {
            name: e.name.clone(),
            min_occurs: min,
            max_occurs: max,
            type_name: self.type_name(ty),
            source,
            type_is_abstract: self.is_abstract_type(ty),
            has_derived_types: has_derived,
            documentation: e
                .doc
                .clone()
                .or_else(|| self.type_doc(ty).map(str::to_string)),
        }
    }

    /// Elements allowed as children of the last path element, with substitution groups and
    /// wildcards expanded. Abstract elements are replaced by their concrete members. Does
    /// not look at existing siblings: everything the content model allows anywhere is
    /// offered, in content-model order.
    pub fn child_elements(&self, path: &[PathStep]) -> ChildCompletions {
        let Some(r) = self.resolve(path) else {
            return ChildCompletions::default();
        };
        let mut out = ChildCompletions {
            mixed: self.is_mixed(r.effective),
            ..ChildCompletions::default()
        };
        let mut index: HashMap<QName, usize> = HashMap::new();
        let mut push = |out: &mut ChildCompletions, c: ChildElement| match index.get(&c.name) {
            Some(&i) => {
                let prev = &mut out.elements[i];
                prev.min_occurs = prev.min_occurs.saturating_add(c.min_occurs);
                prev.max_occurs = prev.max_occurs.add(c.max_occurs);
            }
            None => {
                index.insert(c.name.clone(), out.elements.len());
                out.elements.push(c);
            }
        };
        for s in self.slots(r.effective) {
            match s {
                Slot::Elem { id, min, max } => {
                    let e = self.elem(id);
                    if !e.is_abstract {
                        push(
                            &mut out,
                            self.child_element(id, min, max, SuggestionSource::Declared),
                        );
                    }
                    if e.global {
                        for m in self.substitution_members(id) {
                            if !self.elem(m).is_abstract {
                                let src = SuggestionSource::Substitution {
                                    head: e.name.clone(),
                                };
                                // Members share the head's position; none is required alone.
                                push(&mut out, self.child_element(m, 0, max, src));
                            }
                        }
                    }
                }
                Slot::Any {
                    wildcard,
                    min: _,
                    max,
                } => {
                    out.wildcards.push(wildcard.clone());
                    if wildcard.process_contents == ProcessContents::Skip {
                        continue;
                    }
                    for id in self.wildcard_elements(wildcard) {
                        push(
                            &mut out,
                            self.child_element(id, 0, max, SuggestionSource::Wildcard),
                        );
                    }
                }
            }
        }
        out
    }

    /// Non-abstract global elements a wildcard admits, by namespace then definition order.
    pub(crate) fn wildcard_elements(&self, w: &Wildcard) -> Vec<ElemId> {
        let mut nss: Vec<&String> = self
            .elements_by_ns
            .keys()
            .filter(|ns| w.namespaces.allows(ns))
            .collect();
        nss.sort();
        nss.into_iter()
            .flat_map(|ns| self.elements_by_ns[ns].iter().copied())
            .filter(|&id| !self.elem(id).is_abstract)
            .collect()
    }

    /// Attributes allowed on the last path element, including `xsi:type` when the declared
    /// type has derived types and `xsi:nil` when the element is nillable.
    pub fn attributes(&self, path: &[PathStep]) -> AttributeCompletions {
        let Some(r) = self.resolve(path) else {
            return AttributeCompletions::default();
        };
        let (attrs, wildcard) = self.effective_attributes(r.effective);
        let mut out = AttributeCompletions::default();
        for a in attrs {
            out.attributes.push(AttributeSuggestion {
                name: a.name,
                required: a.required,
                type_name: a.type_name,
                default: a.default,
                fixed: a.fixed,
                source: SuggestionSource::Declared,
                documentation: a.doc,
            });
        }
        if let Some(w) = &wildcard
            && w.process_contents != ProcessContents::Skip
        {
            let mut nss: Vec<&String> = self
                .attributes_by_ns
                .keys()
                .filter(|ns| w.namespaces.allows(ns))
                .collect();
            nss.sort();
            for q in nss.into_iter().flat_map(|ns| &self.attributes_by_ns[ns]) {
                if out.attributes.iter().any(|a| a.name == *q) {
                    continue;
                }
                if let Some(d) = self.global_attributes.get(q) {
                    let a = self.eff_attr(d, None, None);
                    out.attributes.push(AttributeSuggestion {
                        name: a.name,
                        required: false,
                        type_name: a.type_name,
                        default: a.default,
                        fixed: a.fixed,
                        source: SuggestionSource::Wildcard,
                        documentation: a.doc,
                    });
                }
            }
        }
        out.wildcard = wildcard;
        let block = r.elem.map(|e| self.elem(e).block).unwrap_or_default();
        if self
            .xsi_type_options(r.declared, block)
            .iter()
            .any(|&k| k != r.declared)
        {
            out.attributes
                .push(xsi_attr("type", self.is_abstract_type(r.declared), "QName"));
        }
        if r.elem.is_some_and(|e| self.elem(e).nillable) {
            out.attributes.push(xsi_attr("nil", false, "boolean"));
        }
        out
    }

    /// Enumeration values for the text content of the last path element.
    pub fn text_values(&self, path: &[PathStep]) -> Vec<EnumValue> {
        match self.resolve(path) {
            Some(r) if self.has_simple_value(r.effective) => {
                enum_values(self.enumerations(r.effective))
            }
            _ => Vec::new(),
        }
    }

    /// Enumeration values for an attribute of the last path element. `xsi:type` values come
    /// from [`SchemaModel::xsi_type_candidates`] instead.
    pub fn attribute_values(&self, path: &[PathStep], attribute: &QName) -> Vec<EnumValue> {
        if attribute.ns == XSI_NS && attribute.local == "nil" {
            return enum_values(vec![("true".into(), None), ("false".into(), None)]);
        }
        let Some(r) = self.resolve(path) else {
            return Vec::new();
        };
        let (attrs, wildcard) = self.effective_attributes(r.effective);
        let key = match attrs.into_iter().find(|a| a.name == *attribute) {
            Some(a) => a.type_key,
            None => match (wildcard, self.global_attributes.get(attribute)) {
                (Some(w), Some(d))
                    if w.process_contents != ProcessContents::Skip
                        && w.namespaces.allows(&attribute.ns) =>
                {
                    self.eff_attr(d, None, None).type_key
                }
                _ => return Vec::new(),
            },
        };
        enum_values(self.enumerations(key))
    }

    /// Concrete types allowed as `xsi:type` on the last path element: the declared type (if
    /// not abstract) and every transitively derived, non-abstract type whose derivation is
    /// not blocked. An `xsi:type` already present on the last step is ignored.
    pub fn xsi_type_candidates(&self, path: &[PathStep]) -> Vec<TypeSuggestion> {
        let Some(r) = self.resolve(path) else {
            return Vec::new();
        };
        let block = r.elem.map(|e| self.elem(e).block).unwrap_or_default();
        self.xsi_type_options(r.declared, block)
            .into_iter()
            .filter_map(|k| {
                Some(TypeSuggestion {
                    name: self.type_name(k)?,
                    is_declared: k == r.declared,
                    documentation: self.type_doc(k).map(str::to_string),
                })
            })
            .collect()
    }

    /// Hover information for the last path element.
    pub fn element_info(&self, path: &[PathStep]) -> Option<ElementInfo> {
        let r = self.resolve(path)?;
        let last = path.last()?;
        let (is_abstract, nillable, doc) = match r.elem {
            Some(e) => {
                let e = self.elem(e);
                (e.is_abstract, e.nillable, e.doc.clone())
            }
            None => (false, false, None),
        };
        Some(ElementInfo {
            name: last.name.clone(),
            declared_type: self.type_name(r.declared),
            actual_type: self.type_name(r.effective),
            min_occurs: r.min,
            max_occurs: r.max,
            is_abstract,
            type_is_abstract: self.is_abstract_type(r.effective),
            nillable,
            documentation: doc.or_else(|| self.type_doc(r.effective).map(str::to_string)),
        })
    }

    /// Hover information for a named type, including built-in `xs:` types.
    pub fn type_info(&self, name: &QName) -> Option<TypeInfo> {
        let key = self.resolve_type(name);
        match key {
            TypeKey::Def(id) => {
                let td = self.ty(id);
                Some(TypeInfo {
                    name: name.clone(),
                    base: self.base_key(id).and_then(|k| self.type_name(k)),
                    derivation: td.derivation,
                    is_abstract: td.is_abstract,
                    is_complex: matches!(td.kind, TypeKind::Complex { .. }),
                    enumerations: enum_values(self.enumerations(key)),
                    patterns: td.facets.patterns.clone(),
                    documentation: td.doc.clone(),
                })
            }
            TypeKey::Builtin(b) => Some(TypeInfo {
                name: name.clone(),
                base: b
                    .base()
                    .map(|b| self.type_name(TypeKey::Builtin(b)))
                    .unwrap_or_default(),
                derivation: b.base().map(|_| Derivation::Restriction),
                is_abstract: false,
                is_complex: b.is_any_type(),
                enumerations: enum_values(self.enumerations(key)),
                patterns: Vec::new(),
                documentation: None,
            }),
            TypeKey::Unknown => None,
        }
    }
}

fn xsi_attr(local: &str, required: bool, type_local: &str) -> AttributeSuggestion {
    AttributeSuggestion {
        name: QName::new(XSI_NS, local),
        required,
        type_name: Some(QName::new(crate::soap::XSD_NS, type_local)),
        default: None,
        fixed: None,
        source: SuggestionSource::Xsi,
        documentation: None,
    }
}

fn enum_values(v: Vec<(String, Option<String>)>) -> Vec<EnumValue> {
    v.into_iter()
        .map(|(value, documentation)| EnumValue {
            value,
            documentation,
        })
        .collect()
}
