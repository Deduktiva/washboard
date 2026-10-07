//! XML names, QName splitting and in-scope namespace maps.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::model::QName;

use super::XML_NS;

/// XML whitespace (`S` production): space, tab, CR, LF. Not Unicode whitespace.
pub(crate) fn is_xml_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Splits `p:local` into `(Some("p"), "local")`; a name without a colon has no prefix.
pub(crate) fn split_qname(raw: &str) -> (Option<&str>, &str) {
    match raw.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, raw),
    }
}

/// If `attr` declares a namespace, the prefix it binds (`""` for the default namespace).
pub(crate) fn xmlns_prefix(attr: &str) -> Option<&str> {
    if attr == "xmlns" {
        Some("")
    } else {
        attr.strip_prefix("xmlns:")
    }
}

/// Resolves predefined entities and character references; anything unresolvable stays as written.
///
/// Used for attribute values the editor needs to interpret (namespace URIs, `xsi:type`), where a
/// best-effort reading of a possibly broken document is more useful than an error.
pub(crate) fn unescape_lossy(raw: &str) -> Cow<'_, str> {
    if !raw.contains('&') {
        return Cow::Borrowed(raw);
    }
    quick_xml::escape::unescape(raw).unwrap_or(Cow::Borrowed(raw))
}

/// The namespace bindings in scope at an element: prefix → namespace URI.
///
/// The default namespace is stored under the empty prefix; an empty URI there means "no
/// namespace" (`xmlns=""`). The `xml` prefix is always bound. Used to resolve element names,
/// attribute names and QName-valued attributes such as `xsi:type`, and by completion to find
/// the prefix to insert for a namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceMap {
    bindings: BTreeMap<String, String>,
}

impl Default for NamespaceMap {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert("xml".to_owned(), XML_NS.to_owned());
        Self { bindings }
    }
}

impl NamespaceMap {
    /// Adds or overrides a binding; `prefix` is `""` for the default namespace.
    pub fn bind(&mut self, prefix: impl Into<String>, ns: impl Into<String>) {
        self.bindings.insert(prefix.into(), ns.into());
    }

    /// Namespace bound to `prefix` (`""` = default namespace; `Some("")` means none).
    pub fn namespace(&self, prefix: &str) -> Option<&str> {
        self.bindings.get(prefix).map(String::as_str)
    }

    /// The default namespace, `""` if none is in scope.
    pub fn default_namespace(&self) -> &str {
        self.namespace("").unwrap_or("")
    }

    /// How to refer to `ns` in an element name or a QName value such as `xsi:type`: `""` if it
    /// is the default namespace (no prefix needed), else the alphabetically first prefix bound
    /// to it. `None` means completion must add a declaration (PLAN §5.2).
    pub fn prefix_for(&self, ns: &str) -> Option<&str> {
        if !ns.is_empty() && self.default_namespace() == ns {
            return Some("");
        }
        self.declared_prefix_for(ns)
    }

    /// A non-empty prefix bound to `ns`. Use this for attributes, which never take the default
    /// namespace.
    pub fn declared_prefix_for(&self, ns: &str) -> Option<&str> {
        if ns.is_empty() {
            return None;
        }
        self.bindings
            .iter()
            .find(|(p, u)| !p.is_empty() && u.as_str() == ns)
            .map(|(p, _)| p.as_str())
    }

    /// All bindings in prefix order, including `xml` and the default namespace (under `""`).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.bindings.iter().map(|(p, u)| (p.as_str(), u.as_str()))
    }

    /// Resolves an element name; unprefixed names take the default namespace.
    /// `None` if the prefix is not declared.
    pub fn resolve_element(&self, raw: &str) -> Option<QName> {
        self.resolve(raw, true)
    }

    /// Resolves an attribute name; unprefixed attributes are in no namespace.
    pub fn resolve_attribute(&self, raw: &str) -> Option<QName> {
        self.resolve(raw, false)
    }

    /// Resolves a QName-valued attribute (e.g. `xsi:type`). As in XSD, an unprefixed value takes
    /// the default namespace. Surrounding whitespace is ignored.
    pub fn resolve_qname_value(&self, raw: &str) -> Option<QName> {
        self.resolve(
            raw.trim_matches(|c: char| c.is_ascii() && is_xml_ws(c as u8)),
            true,
        )
    }

    fn resolve(&self, raw: &str, use_default: bool) -> Option<QName> {
        match split_qname(raw) {
            (Some(prefix), local) => {
                let ns = self.namespace(prefix)?;
                // `xmlns:p=""` is not a binding in XML 1.0; treat it as undeclared.
                (!ns.is_empty()).then(|| QName::new(ns, local))
            }
            (None, local) => {
                let ns = if use_default {
                    self.default_namespace()
                } else {
                    ""
                };
                Some(QName::new(ns, local))
            }
        }
    }
}
