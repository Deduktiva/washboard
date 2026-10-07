//! The request validation pipeline (PLAN §4 "Validation semantics").
//!
//! [`validate_request`] runs the six steps in order and stops as soon as a step makes the next
//! one meaningless: well-formedness → SOAP 1.1 envelope → operation dispatch per `Body` child
//! → XSD validation of the body and declared header blocks. Everything it reports carries a
//! position in the *request file*, so the editor can underline it and the issues bar can jump
//! to it.
//!
//! # Blocks
//!
//! libxml2 validates a document against a global element declaration, and neither the envelope
//! nor the `Body` is in the project schema. Each block (a `Body` child, or a header block the
//! binding declares) is therefore cut out as a standalone document: the text before it is
//! replaced by newlines and spaces so that every line and column still matches the request
//! file, and the namespace declarations it inherited from its ancestors are added to its start
//! tag. Columns on the start tag's own line shift by the length of those declarations; element
//! positions do not, because they point at the `<`.

use roxmltree::Node;

use crate::diag::{DiagSource, Diagnostic, Severity, TextPos, pos_at_byte};
use crate::model::{OperationRef, QName};
use crate::soap::SOAP11_ENV_NS;
use crate::wsdl::{Dispatch, Wsdl};
use crate::xml;

use super::xsd::CompiledSchema;

/// SOAP 1.2 envelope namespace. Only recognized to give a specific error (PLAN §1).
const SOAP12_ENV_NS: &str = "http://www.w3.org/2003/05/soap-envelope";

/// What validating one request found.
#[derive(Debug, Clone, Default)]
pub struct Validation {
    /// In pipeline order: well-formedness, envelope, dispatch, then schema errors per block.
    pub diagnostics: Vec<Diagnostic>,
    /// The operation each `Body` child dispatched to, in document order. Empty when the
    /// request did not get as far as dispatch, or when nothing matched.
    pub dispatched: Vec<Dispatch>,
}

impl Validation {
    /// Send is blocked while this is true (PLAN §4). Warnings do not block.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// The operation the request will be sent as: the first `Body` child's.
    pub fn operation(&self) -> Option<&OperationRef> {
        self.dispatched.first().map(|d| &d.operation)
    }

    /// `SOAPAction` of [`Self::operation`]; `None` means send `SOAPAction: ""`.
    pub fn soap_action(&self) -> Option<&str> {
        self.dispatched.first()?.soap_action.as_deref()
    }
}

/// Validates one request against the project's WSDL and compiled schema.
///
/// `text` is the decoded request file (see [`crate::xml::decode`]). `hint` is the operation
/// the request was created for; it only picks between operations that share a body element
/// (one portType bound twice) and is otherwise ignored, because the body decides.
pub fn validate_request(
    wsdl: &Wsdl,
    schema: &CompiledSchema,
    text: &str,
    hint: Option<&OperationRef>,
) -> Validation {
    let mut out = Validation::default();

    // 1. Well-formedness. Everything below assumes a tree, so this is fatal.
    if let Err(d) = xml::check_well_formed(text) {
        out.diagnostics.push(d);
        return out;
    }
    let doc = match roxmltree::Document::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            // The checker above accepts a few documents roxmltree rejects (very deep nesting,
            // enormous entity expansion). Report its own message rather than nothing.
            let pos = TextPos {
                line: e.pos().row,
                column: e.pos().col,
            };
            out.diagnostics.push(Diagnostic::error(
                DiagSource::WellFormedness,
                Some(pos),
                format!("not well-formed: {e}"),
            ));
            return out;
        }
    };

    // 2./3. A SOAP 1.1 envelope with a `Body`, and nothing unexpected around it.
    let env = doc.root_element();
    let Some(body) = envelope(&mut out, text, env) else {
        return out;
    };

    // 4. Every `Body` child is an operation's input.
    let mut blocks: Vec<Node<'_, '_>> = Vec::new();
    for child in body.children().filter(Node::is_element) {
        let name = qname(child);
        match wsdl.dispatch(&name, hint) {
            Some(d) => {
                out.dispatched.push(d.clone());
                blocks.push(child);
            }
            None => out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                Some(pos_of(text, child)),
                match wsdl.dispatch_all(&name).is_empty() {
                    true => format!("no operation of this WSDL takes {name} as its request"),
                    // Dispatch only indexes supported operations, so this cannot happen
                    // today; kept so the message stays right if that changes.
                    false => format!("no supported operation takes {name} as its request"),
                },
            )),
        }
    }
    if out.dispatched.is_empty() && out.diagnostics.is_empty() {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            Some(pos_of(text, body)),
            "the SOAP Body is empty; it must contain the operation's request element",
        ));
    }

    // 6. Header blocks the binding declares. Undeclared ones (WS-Security and friends) are
    // not in the project schema, so they are reported but not validated.
    let declared: Vec<&QName> = out
        .dispatched
        .iter()
        .flat_map(|d| d.header_elements.iter())
        .collect();
    let header = env
        .children()
        .filter(Node::is_element)
        .find(|n| in_env(n, "Header"));
    for block in header
        .iter()
        .flat_map(|h| h.children().filter(Node::is_element))
    {
        let name = qname(block);
        if declared.iter().any(|d| **d == name) {
            blocks.push(block);
        } else {
            out.diagnostics.push(Diagnostic::warning(
                DiagSource::Soap,
                Some(pos_of(text, block)),
                format!("{name} is not a header of this operation; it is not validated"),
            ));
        }
    }

    // 5./6. The blocks themselves, against the compiled project schema.
    for block in blocks {
        out.diagnostics
            .extend(schema.validate(standalone(text, block).as_bytes()));
    }
    out
}

/// Checks the envelope's shape and returns its `Body` (PLAN §4 steps 2 and 3).
///
/// This is the SOAP 1.1 envelope schema's content model, checked here rather than by libxml2:
/// the messages name what is wrong instead of reading like a content-model violation, and the
/// positions come from the request text directly. Like that schema, it allows elements from
/// other namespaces after the `Body`.
fn envelope<'a, 'i>(out: &mut Validation, text: &str, env: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    let pos = Some(pos_of(text, env));
    let name = qname(env);
    if name.ns == SOAP12_ENV_NS {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            pos,
            "this is a SOAP 1.2 envelope; washboard supports SOAP 1.1 only",
        ));
        return None;
    }
    if name.ns != SOAP11_ENV_NS || name.local != "Envelope" {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            pos,
            format!("the root element is {name}, not a SOAP 1.1 Envelope"),
        ));
        return None;
    }

    let mut header = None;
    let mut body = None;
    for child in env.children().filter(Node::is_element) {
        let at = Some(pos_of(text, child));
        let name = qname(child);
        if name.ns != SOAP11_ENV_NS {
            if body.is_none() {
                out.diagnostics.push(Diagnostic::error(
                    DiagSource::Soap,
                    at,
                    format!("{name} is not allowed before the SOAP Body"),
                ));
            }
            continue;
        }
        match name.local.as_str() {
            "Header" if body.is_some() => out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                at,
                "the SOAP Header must come before the Body",
            )),
            "Header" if header.is_some() => out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                at,
                "the envelope has more than one SOAP Header",
            )),
            "Header" => header = Some(child),
            "Body" if body.is_some() => out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                at,
                "the envelope has more than one SOAP Body",
            )),
            "Body" => body = Some(child),
            _ => out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                at,
                format!("{name} is not allowed in a SOAP Envelope"),
            )),
        }
    }
    if let Some(h) = header {
        for block in h.children().filter(Node::is_element) {
            if block.tag_name().namespace().is_none() {
                out.diagnostics.push(Diagnostic::error(
                    DiagSource::Soap,
                    Some(pos_of(text, block)),
                    format!(
                        "the header block {} has no namespace; SOAP 1.1 header blocks must be \
                         namespace-qualified",
                        block.tag_name().name()
                    ),
                ));
            }
        }
    }
    if body.is_none() {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            pos,
            "the envelope has no SOAP Body",
        ));
    }
    body
}

fn in_env(n: &Node<'_, '_>, local: &str) -> bool {
    n.tag_name().namespace() == Some(SOAP11_ENV_NS) && n.tag_name().name() == local
}

fn qname(n: Node<'_, '_>) -> QName {
    QName::new(
        n.tag_name().namespace().unwrap_or_default(),
        n.tag_name().name(),
    )
}

/// Position of an element's start tag in the request file.
fn pos_of(text: &str, n: Node<'_, '_>) -> TextPos {
    pos_at_byte(text, n.range().start)
}

/// Cuts `block` out as a standalone document with its line and column numbers preserved and
/// the namespaces it inherited declared on its start tag (module docs).
fn standalone(text: &str, block: Node<'_, '_>) -> String {
    let range = block.range();
    let before = &text[..range.start];
    let line = before.matches('\n').count();
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count();

    let elem = &text[range];
    let name_end = elem
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(elem.len());
    let mut decls = String::new();
    for (prefix, uri) in inherited_namespaces(block, &elem[..name_end], &elem[name_end..]) {
        match prefix {
            Some(p) => decls.push_str(&format!(" xmlns:{p}=\"{}\"", xml::escape_attr(&uri))),
            None => decls.push_str(&format!(" xmlns=\"{}\"", xml::escape_attr(&uri))),
        }
    }

    let mut out = String::with_capacity(line + column + elem.len() + decls.len());
    out.extend(std::iter::repeat_n('\n', line));
    out.extend(std::iter::repeat_n(' ', column));
    out.push_str(&elem[..name_end]);
    out.push_str(&decls);
    out.push_str(&elem[name_end..]);
    out
}

/// In-scope namespaces of `block` that its own start tag does not declare. Re-declaring one it
/// declares itself would be a duplicate attribute, so the start tag's `xmlns` attributes are
/// scanned (roxmltree does not expose them as attributes).
fn inherited_namespaces(
    block: Node<'_, '_>,
    name: &str,
    rest: &str,
) -> Vec<(Option<String>, String)> {
    let tag = format!("{name}{rest}");
    let mut own: Vec<Option<String>> = Vec::new();
    let mut reader = quick_xml::Reader::from_str(&tag);
    let mut buf = Vec::new();
    // The slice is one well-formed element, so the first event is its start tag.
    if let Ok(quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e)) =
        reader.read_event_into(&mut buf)
    {
        for attr in e.attributes().flatten() {
            let key: &str = attr.key.as_ref();
            if key == "xmlns" {
                own.push(None);
            } else if let Some(p) = key.strip_prefix("xmlns:") {
                own.push(Some(p.to_owned()));
            }
        }
    }
    block
        .namespaces()
        .map(|ns| (ns.name().map(str::to_owned), ns.uri().to_owned()))
        .filter(|(prefix, _)| !own.contains(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::wsdl::{self, Sources};

    fn customer() -> (Wsdl, CompiledSchema) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/customer");
        let extra: Vec<PathBuf> = ["CustomerBinding.wsdl", "xsd"]
            .iter()
            .map(|e| root.join(e))
            .collect();
        let w = wsdl::load(
            &Sources::from_disk(&root.join("CustomerService.wsdl"), &extra).expect("sources"),
        );
        let s = CompiledSchema::compile(&w.bundle).expect("fixture bundle compiles");
        (w, s)
    }

    fn run(text: &str) -> Validation {
        let (w, s) = customer();
        validate_request(&w, &s, text, None)
    }

    fn errors(v: &Validation) -> Vec<String> {
        v.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(ToString::to_string)
            .collect()
    }

    const GET_CUSTOMER: &str = concat!(
        "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"\n",
        "                  xmlns:msg=\"urn:example:customer:messages\">\n",
        "  <soapenv:Body>\n",
        "    <msg:GetCustomer><msg:customerId>1</msg:customerId></msg:GetCustomer>\n",
        "  </soapenv:Body>\n",
        "</soapenv:Envelope>\n",
    );

    #[test]
    fn dispatches_and_reports_the_soap_action() {
        let v = run(GET_CUSTOMER);
        assert!(!v.has_errors(), "{:#?}", v.diagnostics);
        assert_eq!(
            v.operation().map(|o| o.operation.as_str()),
            Some("GetCustomer")
        );
        assert_eq!(
            v.soap_action(),
            Some("urn:example:customer:service/GetCustomer")
        );
    }

    #[test]
    fn stops_at_the_first_well_formedness_error() {
        let v = run("<soapenv:Envelope><Body>\n");
        let e = errors(&v);
        assert_eq!(e.len(), 1, "{e:#?}");
        assert!(e[0].contains("not well-formed"), "{e:#?}");
    }

    #[test]
    fn soap12_and_other_roots_get_their_own_error() {
        let v = run(
            "<e:Envelope xmlns:e=\"http://www.w3.org/2003/05/soap-envelope\"><e:Body/></e:Envelope>",
        );
        assert_eq!(errors(&v).len(), 1);
        assert!(errors(&v)[0].contains("SOAP 1.2"), "{:#?}", errors(&v));

        let v = run("<msg:GetCustomer xmlns:msg=\"urn:example:customer:messages\"/>");
        assert!(
            errors(&v)[0].contains("not a SOAP 1.1 Envelope"),
            "{:#?}",
            errors(&v)
        );
    }

    #[test]
    fn envelope_shape_is_checked() {
        let env = |inner: &str| {
            format!(
                "<e:Envelope xmlns:e=\"http://schemas.xmlsoap.org/soap/envelope/\">{inner}</e:Envelope>"
            )
        };
        let cases = [
            ("<e:Header/>", "no SOAP Body"),
            ("<e:Body/><e:Header/>", "must come before the Body"),
            ("<e:Body/><e:Body/>", "more than one SOAP Body"),
            ("<e:Fault/><e:Body/>", "not allowed in a SOAP Envelope"),
            ("<e:Body/>", "Body is empty"),
            (
                "<e:Header><h/></e:Header><e:Body/>",
                "must be namespace-qualified",
            ),
        ];
        for (inner, needle) in cases {
            let v = run(&env(inner));
            assert!(
                errors(&v).iter().any(|e| e.contains(needle)),
                "{inner}: expected {needle:?}, got {:#?}",
                errors(&v)
            );
        }
    }

    #[test]
    fn undeclared_header_blocks_are_a_warning_only() {
        let text = GET_CUSTOMER.replace(
            "  <soapenv:Body>",
            "  <soapenv:Header><wsse:Security xmlns:wsse=\"urn:example:wsse\"/></soapenv:Header>\n  <soapenv:Body>",
        );
        let v = run(&text);
        assert!(!v.has_errors(), "{:#?}", v.diagnostics);
        let w: Vec<_> = v
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        assert_eq!(w.len(), 1, "{:#?}", v.diagnostics);
        assert!(
            w[0].message.contains("not a header of this operation"),
            "{w:#?}"
        );
    }

    #[test]
    fn declared_header_blocks_are_validated() {
        let text = GET_CUSTOMER.replace(
            "  <soapenv:Body>",
            "  <soapenv:Header>\n    <msg:RequestContext>\n      <msg:nope/>\n    </msg:RequestContext>\n  </soapenv:Header>\n  <soapenv:Body>",
        );
        let v = run(&text);
        let e = errors(&v);
        assert!(
            e.iter().any(|m| m.starts_with("5:7:")),
            "expected an error at 5:7: {e:#?}"
        );
    }

    /// Cutting a block out keeps its line *and* column, and does not duplicate a namespace
    /// declaration the block makes itself.
    #[test]
    fn block_positions_and_namespaces_survive_the_cut() {
        let text = concat!(
            "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"\n",
            "                  xmlns:cus=\"urn:example:customer\">\n",
            "  <soapenv:Body>\n",
            "      <msg:CreateOrder xmlns:msg=\"urn:example:customer:messages\">\n",
            "        <cus:Customer><cus:id>1</cus:id><cus:nope/></cus:Customer>\n",
            "      </msg:CreateOrder>\n",
            "  </soapenv:Body>\n",
            "</soapenv:Envelope>\n",
        );
        let v = run(text);
        let e = errors(&v);
        assert!(!e.is_empty(), "the body is invalid");
        // Line 5, and the column of `<cus:nope/>`; nothing about a duplicate xmlns:msg.
        assert!(
            e.iter().any(|m| m.starts_with("5:41:")),
            "expected an error at 5:41 (the `<` of `<cus:nope/>`), got {e:#?}"
        );
        assert!(!e.iter().any(|m| m.contains("well-formed")), "{e:#?}");
    }

    #[test]
    fn unknown_body_element_is_an_error_with_a_position() {
        let v = run(&GET_CUSTOMER.replace("GetCustomer", "DeleteCustomer"));
        let e = errors(&v);
        assert_eq!(e.len(), 1, "{e:#?}");
        assert!(e[0].starts_with("4:5: error: no operation"), "{e:#?}");
        assert!(v.operation().is_none());
    }
}
