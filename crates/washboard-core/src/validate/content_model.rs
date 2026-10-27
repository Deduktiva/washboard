//! Explains libxml2's "The content model is not determinist" compile error, which names only
//! the complex type. The schema violates Unique Particle Attribution (XSD 1.0 §3.8.6): an
//! element in an instance could match more than one particle. In vendor schemas that is almost
//! always one element declared twice with only optional content between, or a wildcard next to
//! a declaration it also matches, so the explanation lists those, with their lines, for the
//! user to fix the schema.
//!
//! Best effort, on the schema text: group references and `xs:extension` bases are followed
//! within the same document only. Lines are those of the bundle document, which are the source
//! file's (inline schemas are extracted in place).

use std::collections::HashSet;

use roxmltree::Node;

use crate::soap::XSD_NS;
use crate::xml::parse_wsdl_or_xsd;

/// Text to append to the error for the complex type whose start tag begins at byte `at` of
/// `text`. `None` if that is not a complex type.
pub(super) fn explain(text: &str, at: usize) -> Option<String> {
    let doc = parse_wsdl_or_xsd(text).ok()?;
    let ty = doc
        .descendants()
        .find(|n| n.range().start == at && n.has_tag_name((XSD_NS, "complexType")))?;
    let mut particles = Particles::default();
    particles.walk(ty);
    let line = |n: Node<'_, '_>| doc.text_pos_at(n.range().start).row;

    let mut out = String::new();
    let mut seen: Vec<(Option<&str>, &str)> = Vec::new();
    for (i, (name, _)) in particles.elements.iter().enumerate() {
        if seen.contains(name) {
            continue;
        }
        seen.push(*name);
        let lines: Vec<String> = particles.elements[i..]
            .iter()
            .filter(|(n, _)| n == name)
            .map(|&(_, node)| line(node).to_string())
            .collect();
        if lines.len() > 1 {
            out.push_str(&format!(
                " Element `{}` is declared more than once in this type (lines {}), so a `{}` in a \
                 request could match more than one declaration.",
                name.1,
                lines.join(", "),
                name.1
            ));
        }
    }
    if !out.is_empty() {
        out.push_str(
            " Fix the schema: remove or merge the duplicate declarations, or make an element \
             between them required.",
        );
        return Some(out);
    }
    if !particles.wildcards.is_empty() {
        let lines: Vec<String> = particles
            .wildcards
            .iter()
            .map(|&n| line(n).to_string())
            .collect();
        return Some(format!(
            " Its wildcard (`xs:any`, line {}) can match the same elements as a declaration next \
             to it. Fix the schema: narrow the wildcard's `namespace`, or make the content \
             between them required.",
            lines.join(", ")
        ));
    }
    Some(
        " An element in a request could match more than one particle of this type \
         (Unique Particle Attribution). Fix the schema so that each element can only match one \
         declaration."
            .into(),
    )
}

/// The element declarations and wildcards of a content model, in document order.
#[derive(Default)]
struct Particles<'a, 'input> {
    /// Expanded name (namespace, local name) and declaration.
    elements: Vec<((Option<&'a str>, &'a str), Node<'a, 'input>)>,
    wildcards: Vec<Node<'a, 'input>>,
    /// Groups and base types already walked, by node position; guards against cycles.
    visited: HashSet<usize>,
}

impl<'a, 'input> Particles<'a, 'input> {
    fn walk(&mut self, node: Node<'a, 'input>) {
        if !self.visited.insert(node.range().start) {
            return;
        }
        for child in node
            .children()
            .filter(|c| c.tag_name().namespace() == Some(XSD_NS))
        {
            match child.tag_name().name() {
                // A local element's own type is a content model of its own.
                "element" => {
                    if let Some(name) = element_name(child) {
                        self.elements.push((name, child));
                    }
                }
                "any" => self.wildcards.push(child),
                "sequence" | "choice" | "all" | "complexContent" | "restriction" => {
                    self.walk(child)
                }
                "extension" => {
                    if let Some(base) = child
                        .attribute("base")
                        .and_then(|b| global(child, "complexType", b))
                    {
                        self.walk(base);
                    }
                    self.walk(child);
                }
                "group" => {
                    if let Some(group) = child
                        .attribute("ref")
                        .and_then(|r| global(child, "group", r))
                    {
                        self.walk(group);
                    }
                }
                _ => {}
            }
        }
    }
}

/// A declaration's expanded name: `ref` resolved, or `name` qualified per `form` and
/// `elementFormDefault`.
fn element_name<'a>(el: Node<'a, '_>) -> Option<(Option<&'a str>, &'a str)> {
    if let Some(r) = el.attribute("ref") {
        return resolve(el, r);
    }
    let name = el.attribute("name")?;
    let schema = schema_of(el)?;
    let qualified = el
        .attribute("form")
        .or_else(|| schema.attribute("elementFormDefault"))
        == Some("qualified");
    Some((
        qualified
            .then(|| schema.attribute("targetNamespace"))
            .flatten(),
        name,
    ))
}

/// The top-level `xs:<kind>` that the QName `qname` (in scope at `at`) names, if it is in the
/// same schema.
fn global<'a, 'input>(at: Node<'a, 'input>, kind: &str, qname: &str) -> Option<Node<'a, 'input>> {
    let (ns, local) = resolve(at, qname)?;
    let schema = schema_of(at)?;
    if ns != schema.attribute("targetNamespace") {
        return None;
    }
    schema
        .children()
        .find(|c| c.has_tag_name((XSD_NS, kind)) && c.attribute("name") == Some(local))
}

fn resolve<'a>(at: Node<'a, '_>, qname: &'a str) -> Option<(Option<&'a str>, &'a str)> {
    match qname.split_once(':') {
        Some((prefix, local)) => Some((Some(at.lookup_namespace_uri(Some(prefix))?), local)),
        None => Some((at.lookup_namespace_uri(None), qname)),
    }
}

fn schema_of<'a, 'input>(n: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    n.ancestors().find(|a| a.has_tag_name((XSD_NS, "schema")))
}
