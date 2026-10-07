//! Schema components as parsed from the XSD documents.
//!
//! References between components stay QNames and are resolved through the global indexes at
//! query time (cheap hash lookups). That keeps the parser independent of document order and
//! makes a dangling reference a missing completion instead of a build failure.

use crate::model::QName;

/// Index of an element declaration (global or local) in [`super::SchemaModel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ElemId(pub(crate) u32);

/// Index of a type definition (named or anonymous) in [`super::SchemaModel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TypeId(pub(crate) u32);

/// `maxOccurs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaxOccurs {
    Bounded(u32),
    Unbounded,
}

impl MaxOccurs {
    pub(crate) fn mul(self, other: MaxOccurs) -> MaxOccurs {
        match (self, other) {
            (MaxOccurs::Bounded(0), _) | (_, MaxOccurs::Bounded(0)) => MaxOccurs::Bounded(0),
            (MaxOccurs::Bounded(a), MaxOccurs::Bounded(b)) => {
                MaxOccurs::Bounded(a.saturating_mul(b))
            }
            _ => MaxOccurs::Unbounded,
        }
    }

    pub(crate) fn add(self, other: MaxOccurs) -> MaxOccurs {
        match (self, other) {
            (MaxOccurs::Bounded(a), MaxOccurs::Bounded(b)) => {
                MaxOccurs::Bounded(a.saturating_add(b))
            }
            _ => MaxOccurs::Unbounded,
        }
    }

    pub(crate) fn is_zero(self) -> bool {
        self == MaxOccurs::Bounded(0)
    }
}

/// A set of derivation methods, as used by `block`, `final`, `blockDefault`, `finalDefault`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct DerivationSet {
    pub extension: bool,
    pub restriction: bool,
    pub substitution: bool,
    pub list: bool,
    pub union: bool,
}

impl DerivationSet {
    pub(crate) fn parse(value: &str) -> DerivationSet {
        let mut s = DerivationSet::default();
        for token in value.split_ascii_whitespace() {
            match token {
                "#all" => {
                    return DerivationSet {
                        extension: true,
                        restriction: true,
                        substitution: true,
                        list: true,
                        union: true,
                    };
                }
                "extension" => s.extension = true,
                "restriction" => s.restriction = true,
                "substitution" => s.substitution = true,
                "list" => s.list = true,
                "union" => s.union = true,
                _ => {}
            }
        }
        s
    }

    pub(crate) fn union_with(self, o: DerivationSet) -> DerivationSet {
        DerivationSet {
            extension: self.extension || o.extension,
            restriction: self.restriction || o.restriction,
            substitution: self.substitution || o.substitution,
            list: self.list || o.list,
            union: self.union || o.union,
        }
    }

    pub(crate) fn blocks(self, method: Derivation) -> bool {
        match method {
            Derivation::Extension => self.extension,
            Derivation::Restriction => self.restriction,
            Derivation::List => self.list,
            Derivation::Union => self.union,
        }
    }
}

/// How a type is derived from its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Derivation {
    Extension,
    Restriction,
    List,
    Union,
}

/// `processContents` of a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessContents {
    Strict,
    Lax,
    Skip,
}

/// The `namespace` constraint of `xs:any` / `xs:anyAttribute`. Namespace `""` is "absent"
/// (`##local`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NamespaceConstraint {
    /// `##any`.
    Any,
    /// `##other`: any namespace except the given target namespace, and not absent.
    Not(String),
    /// An explicit list; `##targetNamespace` and `##local` are already resolved.
    List(Vec<String>),
}

impl NamespaceConstraint {
    pub fn allows(&self, ns: &str) -> bool {
        match self {
            NamespaceConstraint::Any => true,
            NamespaceConstraint::Not(tns) => !ns.is_empty() && ns != tns,
            NamespaceConstraint::List(list) => list.iter().any(|n| n == ns),
        }
    }
}

/// An `xs:any` or `xs:anyAttribute`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Wildcard {
    pub namespaces: NamespaceConstraint,
    pub process_contents: ProcessContents,
}

#[derive(Debug, Clone)]
pub(crate) struct Particle {
    pub min: u32,
    pub max: MaxOccurs,
    pub term: Term,
}

#[derive(Debug, Clone)]
pub(crate) enum Term {
    /// A local element declaration.
    Element(ElemId),
    /// `<xs:element ref="…">`, resolved through the global element index.
    ElementRef(QName),
    /// `<xs:group ref="…">`.
    GroupRef(QName),
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
    All(Vec<Particle>),
    Any(Wildcard),
}

/// The type of an element or attribute as written in the schema.
#[derive(Debug, Clone)]
pub(crate) enum TypeRef {
    Named(QName),
    Anonymous(TypeId),
    /// No `type` and no inline type: the head's type for substitution group members,
    /// `xs:anyType` (elements) or `xs:anySimpleType` (attributes) otherwise.
    Default,
}

/// The base of a type definition.
#[derive(Debug, Clone)]
pub(crate) enum BaseRef {
    Named(QName),
    /// The original definition replaced by an `xs:redefine` (its QName now names the
    /// redefinition, so it can only be reached by id).
    Id(TypeId),
}

#[derive(Debug, Clone)]
pub(crate) struct ElementDecl {
    pub name: QName,
    pub type_ref: TypeRef,
    pub global: bool,
    pub is_abstract: bool,
    pub nillable: bool,
    pub default: Option<String>,
    pub fixed: Option<String>,
    /// Substitution group heads (XSD 1.0 allows one; 1.1 a list).
    pub substitution_heads: Vec<QName>,
    pub block: DerivationSet,
    pub final_: DerivationSet,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttrUse {
    Optional,
    Required,
    Prohibited,
}

#[derive(Debug, Clone)]
pub(crate) struct AttributeDecl {
    pub name: QName,
    pub type_ref: TypeRef,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum AttrItem {
    Local {
        decl: AttributeDecl,
        use_: AttrUse,
    },
    Ref {
        name: QName,
        use_: AttrUse,
        default: Option<String>,
        fixed: Option<String>,
    },
    Group(QName),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Facets {
    /// Enumeration values with their documentation.
    pub enumerations: Vec<(String, Option<String>)>,
    pub patterns: Vec<String>,
    pub min_inclusive: Option<String>,
    pub max_inclusive: Option<String>,
    pub min_exclusive: Option<String>,
    pub max_exclusive: Option<String>,
    pub length: Option<String>,
    pub min_length: Option<String>,
    pub max_length: Option<String>,
    pub total_digits: Option<String>,
    pub fraction_digits: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum SimpleVariety {
    /// Derived by restriction (the facets live on the [`TypeDef`]).
    Atomic,
    List(TypeRef),
    Union(Vec<TypeRef>),
}

#[derive(Debug, Clone)]
pub(crate) enum Content {
    /// No element children and no text.
    Empty,
    /// `xs:simpleContent`: text typed by the base chain, plus attributes.
    Simple,
    /// Element content. Extension prepends the base type's content.
    Elements(Particle),
}

#[derive(Debug, Clone)]
pub(crate) enum TypeKind {
    Simple(SimpleVariety),
    Complex {
        content: Content,
        mixed: bool,
        attributes: Vec<AttrItem>,
        any_attribute: Option<Wildcard>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct TypeDef {
    /// `None` for anonymous types.
    pub name: Option<QName>,
    /// For anonymous types: the element or attribute that declares it (for messages).
    pub context: Option<QName>,
    pub kind: TypeKind,
    pub base: Option<BaseRef>,
    pub derivation: Option<Derivation>,
    pub facets: Facets,
    pub is_abstract: bool,
    pub block: DerivationSet,
    pub final_: DerivationSet,
    pub doc: Option<String>,
}

impl TypeDef {
    pub(crate) fn is_complex(&self) -> bool {
        matches!(self.kind, TypeKind::Complex { .. })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GroupDef {
    /// The group's model group (`sequence`/`choice`/`all`), occurring once.
    pub particle: Option<Particle>,
}

#[derive(Debug, Clone)]
pub(crate) struct AttrGroupDef {
    pub items: Vec<AttrItem>,
    pub any_attribute: Option<Wildcard>,
}
