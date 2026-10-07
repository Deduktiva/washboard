//! The request validation pipeline (PLAN §4 "Validation semantics").
//!
//! [`validate_request`] runs well-formedness → SOAP 1.1 root → dispatch of every `Body` child →
//! **one** libxml2 pass over the whole document, and stops as soon as a step makes the next one
//! meaningless. Everything it reports carries a position in the request text itself.
//!
//! The libxml2 pass runs against a [`RequestSchema`]: the project's bundle plus a shipped SOAP
//! 1.1 envelope schema whose `Header` and `Body` hold lax wildcards. The envelope's shape is
//! checked by that schema, every header or body block the project schema declares globally is
//! validated fully, and blocks it does not know (WS-Security and friends) are left alone. No
//! block is cut out of the document, so there is nothing to re-declare and no line mapping
//! beyond the start-tag adjustment in [`super::xsd`].

use roxmltree::Node;

use crate::diag::{DiagSource, Diagnostic, Severity, TextPos, pos_at_byte};
use crate::model::{OperationRef, QName, SchemaBundle, SchemaDoc, SchemaOrigin};
use crate::soap::{SOAP11_ENV_NS, SOAP12_ENV_NS, XSD_NS};
use crate::wsdl::{Dispatch, Wsdl};
use crate::xml;

use super::xsd::{CompiledSchema, StripAttributes};

/// The SOAP 1.1 envelope schema shipped with washboard (never fetched).
const ENVELOPE_XSD: &str = include_str!("soap-envelope-1.1.xsd");
/// Bundle URI of [`ENVELOPE_XSD`].
pub const ENVELOPE_URI: &str = "washboard:/soap/envelope-1.1.xsd";
/// Bundle URI and namespace of the generated root importing the project root and the envelope.
pub const REQUEST_ROOT_URI: &str = "washboard:/request-root.xsd";
const REQUEST_ROOT_NS: &str = "urn:washboard:request-root";

/// The project's schema plus the SOAP 1.1 envelope, compiled once per project (in the app, in
/// the background on open) and reused for every request.
#[derive(Debug)]
pub struct RequestSchema {
    schema: CompiledSchema,
}

impl RequestSchema {
    /// Compiles [`request_bundle`]`(bundle)`. Errors are [`CompiledSchema::compile`]'s.
    pub fn compile(bundle: &SchemaBundle) -> Result<Self, Vec<Diagnostic>> {
        CompiledSchema::compile(&request_bundle(bundle)).map(|schema| Self { schema })
    }

    /// Compile warnings, for the import report.
    pub fn warnings(&self) -> &[Diagnostic] {
        self.schema.warnings()
    }
}

/// `bundle` with the SOAP 1.1 envelope schema added and a new root importing both.
///
/// libxml2 imports a namespace only once, so a project that brings its own copy of the
/// envelope namespace (some WSDLs import it for faults) keeps it, and ours is not added.
pub fn request_bundle(bundle: &SchemaBundle) -> SchemaBundle {
    let mut out = bundle.clone();
    let own_envelope = bundle.docs.iter().any(|d| d.target_ns == SOAP11_ENV_NS);
    if !own_envelope {
        out.docs.push(SchemaDoc {
            uri: ENVELOPE_URI.to_owned(),
            target_ns: SOAP11_ENV_NS.to_owned(),
            origin: SchemaOrigin::Generated,
            text: ENVELOPE_XSD.to_owned(),
        });
    }
    let root_ns = bundle
        .get(&bundle.root)
        .map(|d| d.target_ns.clone())
        .unwrap_or_default();
    let mut root = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xs:schema xmlns:xs=\"{XSD_NS}\" targetNamespace=\"{REQUEST_ROOT_NS}\">\n"
    );
    let import = |ns: &str, uri: &str| match ns {
        "" => format!(
            "  <xs:import schemaLocation=\"{}\"/>\n",
            xml::escape_attr(uri)
        ),
        ns => format!(
            "  <xs:import namespace=\"{}\" schemaLocation=\"{}\"/>\n",
            xml::escape_attr(ns),
            xml::escape_attr(uri)
        ),
    };
    if !bundle.root.is_empty() {
        root.push_str(&import(&root_ns, &bundle.root));
    }
    if !own_envelope {
        root.push_str(&import(SOAP11_ENV_NS, ENVELOPE_URI));
    }
    root.push_str("</xs:schema>\n");
    out.docs.push(SchemaDoc {
        uri: REQUEST_ROOT_URI.to_owned(),
        target_ns: REQUEST_ROOT_NS.to_owned(),
        origin: SchemaOrigin::Generated,
        text: root,
    });
    out.root = REQUEST_ROOT_URI.to_owned();
    out
}

/// What validating one request found.
#[derive(Debug, Clone, Default)]
pub struct Validation {
    /// In pipeline order: well-formedness or root, dispatch, then the schema pass, then
    /// warnings about missing headers.
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

/// Validates one request against the project's WSDL and its [`RequestSchema`].
///
/// `text` is the decoded request file (see [`crate::xml::decode`]). `hint` is the operation
/// the request was created for; it only picks between operations that share a body element
/// (one portType bound twice) and is otherwise ignored, because the body decides.
pub fn validate_request(
    wsdl: &Wsdl,
    schema: &RequestSchema,
    text: &str,
    hint: Option<&OperationRef>,
) -> Validation {
    let mut out = Validation::default();

    // 1. Well-formedness: the same libxml2 parse the editor uses. Fatal.
    if let Err(d) = xml::check_well_formed(text) {
        out.diagnostics.push(d);
        return out;
    }
    let doc = match roxmltree::Document::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            // libxml2 accepts a few documents roxmltree rejects (very deep nesting, large
            // entity expansion). Without a tree there is no dispatch, so report it.
            let pos = TextPos {
                line: e.pos().row,
                column: e.pos().col,
            };
            out.diagnostics.push(Diagnostic::error(
                DiagSource::WellFormedness,
                Some(pos),
                format!("cannot be read as a SOAP request: {e}"),
            ));
            return out;
        }
    };

    // 2. A SOAP 1.1 envelope. Anything else would only produce "no matching global
    // declaration" from libxml2, so it gets its own message and stops here.
    let env = doc.root_element();
    let name = qname(env);
    let pos = Some(pos_of(text, env));
    if name.ns == SOAP12_ENV_NS {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            pos,
            "this is a SOAP 1.2 envelope; washboard supports SOAP 1.1 only",
        ));
        return out;
    }
    if name.ns != SOAP11_ENV_NS || name.local != "Envelope" {
        out.diagnostics.push(Diagnostic::error(
            DiagSource::Soap,
            pos,
            format!("the root element is {name}, not a SOAP 1.1 Envelope"),
        ));
        return out;
    }

    // 3. Every `Body` child is an operation's input. The envelope schema allows anything in
    // the `Body`, so this is the only check that catches an unknown request element. A
    // missing `Body` is left to the schema pass.
    let body = env
        .children()
        .filter(Node::is_element)
        .find(|n| in_env(n, "Body"));
    if let Some(body) = body {
        for child in body.children().filter(Node::is_element) {
            let name = qname(child);
            match wsdl.dispatch(&name, hint) {
                Some(d) => out.dispatched.push(d.clone()),
                None => out.diagnostics.push(Diagnostic::error(
                    DiagSource::Soap,
                    Some(pos_of(text, child)),
                    format!("no operation of this WSDL takes {name} as its request"),
                )),
            }
        }
        if !body.children().any(|n| n.is_element()) {
            out.diagnostics.push(Diagnostic::error(
                DiagSource::Soap,
                Some(pos_of(text, body)),
                "the SOAP Body is empty; it must contain the operation's request element",
            ));
        }
    }

    // 4. One libxml2 pass: envelope shape, headers and body together.
    // SOAP's own header-block attributes are taken out of libxml2's copy first (see
    // `HEADER_BLOCK_ATTRIBUTES`) and checked here instead.
    out.diagnostics.extend(
        schema
            .schema
            .validate_text_stripping(text, &STRIP_HEADER_BLOCK_ATTRIBUTES),
    );
    for block in header_blocks(env) {
        check_must_understand(text, block, &mut out.diagnostics);
    }

    // 5. Headers the binding declares but the request lacks. SOAP makes no header
    // mandatory by itself, so this only warns.
    let present: Vec<QName> = header_blocks(env).map(qname).collect();
    let mut missing: Vec<&QName> = Vec::new();
    for h in out.dispatched.iter().flat_map(|d| &d.header_elements) {
        if !present.contains(h) && !missing.contains(&h) {
            missing.push(h);
        }
    }
    let warnings: Vec<Diagnostic> = missing
        .into_iter()
        .map(|h| {
            Diagnostic::warning(
                DiagSource::Soap,
                pos,
                format!("the operation declares the header {h}, but the request has none"),
            )
        })
        .collect();
    out.diagnostics.extend(warnings);
    out
}

/// SOAP 1.1 attributes any header block may carry (§4.2.2, §4.2.3, §4.1.1).
///
/// A block the project schema declares has a type that usually allows no foreign attributes,
/// so libxml2 would reject these. They are removed from the document libxml2 validates, on
/// every header block, and the one value with a real constraint, `mustUnderstand`, is checked
/// by [`check_must_understand`]. `actor` and `encodingStyle` are URIs, which libxml2 accepts
/// almost unchecked anyway.
const HEADER_BLOCK_ATTRIBUTES: [&str; 3] = ["mustUnderstand", "actor", "encodingStyle"];

const STRIP_HEADER_BLOCK_ATTRIBUTES: StripAttributes<'static> = StripAttributes {
    parent: (SOAP11_ENV_NS, "Header"),
    namespace: SOAP11_ENV_NS,
    names: &HEADER_BLOCK_ATTRIBUTES,
};

/// Element children of the envelope's `Header`s.
fn header_blocks<'a, 'i>(env: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    env.children()
        .filter(|n| in_env(n, "Header"))
        .flat_map(|h| h.children().filter(Node::is_element))
}

/// `soapenv:mustUnderstand` is `0` or `1` (SOAP 1.1 §4.2.3).
fn check_must_understand(text: &str, block: Node<'_, '_>, diags: &mut Vec<Diagnostic>) {
    let Some(value) = block.attribute((SOAP11_ENV_NS, "mustUnderstand")) else {
        return;
    };
    if !matches!(value.trim(), "0" | "1") {
        diags.push(Diagnostic::error(
            DiagSource::Schema,
            Some(pos_of(text, block)),
            format!("soapenv:mustUnderstand must be 0 or 1, not {value:?}"),
        ));
    }
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

/// Position of an element's start tag in the request text.
fn pos_of(text: &str, n: Node<'_, '_>) -> TextPos {
    pos_at_byte(text, n.range().start)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::wsdl::{self, Sources};

    fn customer() -> (Wsdl, RequestSchema) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/customer");
        let extra: Vec<PathBuf> = ["CustomerBinding.wsdl", "xsd"]
            .iter()
            .map(|e| root.join(e))
            .collect();
        let w = wsdl::load(
            &Sources::from_disk(&root.join("CustomerService.wsdl"), &extra).expect("sources"),
        );
        let s = RequestSchema::compile(&w.bundle).expect("fixture bundle compiles");
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

    fn warnings(v: &Validation) -> Vec<String> {
        v.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(ToString::to_string)
            .collect()
    }

    const GET_CUSTOMER: &str = concat!(
        "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"\n",
        "                  xmlns:msg=\"urn:example:customer:messages\">\n",
        "  <soapenv:Header>\n",
        "    <msg:RequestContext><msg:correlationId>c</msg:correlationId></msg:RequestContext>\n",
        "  </soapenv:Header>\n",
        "  <soapenv:Body>\n",
        "    <msg:GetCustomer><msg:customerId>1</msg:customerId></msg:GetCustomer>\n",
        "  </soapenv:Body>\n",
        "</soapenv:Envelope>\n",
    );

    #[test]
    fn dispatches_and_reports_the_soap_action() {
        let v = run(GET_CUSTOMER);
        assert!(v.diagnostics.is_empty(), "{:#?}", v.diagnostics);
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
        assert_eq!(errors(&v).len(), 1, "{:#?}", errors(&v));
        assert!(errors(&v)[0].contains("SOAP 1.2"), "{:#?}", errors(&v));

        let v = run("<msg:GetCustomer xmlns:msg=\"urn:example:customer:messages\"/>");
        assert_eq!(errors(&v).len(), 1, "{:#?}", errors(&v));
        assert!(
            errors(&v)[0].contains("not a SOAP 1.1 Envelope"),
            "{:#?}",
            errors(&v)
        );
    }

    /// The envelope's shape comes from the shipped envelope schema, in the same pass.
    #[test]
    fn envelope_shape_is_checked_by_the_envelope_schema() {
        let env = |inner: &str| {
            format!(
                "<e:Envelope xmlns:e=\"http://schemas.xmlsoap.org/soap/envelope/\" \
                 xmlns:msg=\"urn:example:customer:messages\">{inner}</e:Envelope>"
            )
        };
        let body = "<e:Body><msg:GetCustomer><msg:customerId>1</msg:customerId>\
                    </msg:GetCustomer></e:Body>";
        let cases = [
            ("<e:Header/>".to_owned(), "Body"),
            (format!("{body}<e:Header/>"), "Header"),
            (format!("{body}{body}"), "Body"),
            (format!("<e:Fault/>{body}"), "Fault"),
            (format!("<e:Header><h/></e:Header>{body}"), "h"),
        ];
        for (inner, needle) in cases {
            let v = run(&env(&inner));
            let e = errors(&v);
            assert!(
                e.iter().any(|m| m.contains(needle)),
                "{inner}: expected an error naming {needle:?}, got {e:#?}"
            );
        }
        let v = run(&env("<e:Body/>"));
        assert!(
            errors(&v).iter().any(|e| e.contains("Body is empty")),
            "{:#?}",
            errors(&v)
        );
    }

    #[test]
    fn unknown_header_blocks_are_left_alone() {
        let text = GET_CUSTOMER.replace(
            "  </soapenv:Header>",
            "    <wsse:Security xmlns:wsse=\"urn:example:wsse\" soapenv:mustUnderstand=\"1\">\
             <wsse:Anything/></wsse:Security>\n  </soapenv:Header>",
        );
        let v = run(&text);
        assert!(v.diagnostics.is_empty(), "{:#?}", v.diagnostics);
    }

    #[test]
    fn soap_attributes_are_checked() {
        let text = GET_CUSTOMER.replace(
            "  </soapenv:Header>",
            "    <wsse:Security xmlns:wsse=\"urn:example:wsse\" soapenv:mustUnderstand=\"yes\"/>\n  </soapenv:Header>",
        );
        let e = errors(&run(&text));
        assert!(
            e.iter()
                .any(|m| m.starts_with("5:5:") && m.contains("mustUnderstand")),
            "{e:#?}"
        );
    }

    #[test]
    fn declared_header_blocks_are_validated_in_place() {
        let text = GET_CUSTOMER.replace(
            "<msg:RequestContext><msg:correlationId>c</msg:correlationId></msg:RequestContext>",
            "<msg:RequestContext>\n      <msg:nope/>\n    </msg:RequestContext>",
        );
        let e = errors(&run(&text));
        assert!(
            e.iter().any(|m| m.starts_with("5:7:")),
            "expected an error at 5:7: {e:#?}"
        );
    }

    /// SOAP allows `mustUnderstand`, `actor` and `encodingStyle` on every header block,
    /// including blocks whose declared type has no attribute wildcard.
    #[test]
    fn soap_attributes_are_allowed_on_declared_header_blocks() {
        let text = GET_CUSTOMER.replace(
            "<msg:RequestContext>",
            "<msg:RequestContext soapenv:mustUnderstand=\"1\" \
             soapenv:actor=\"http://schemas.xmlsoap.org/soap/actor/next\">",
        );
        let v = run(&text);
        assert!(v.diagnostics.is_empty(), "{:#?}", v.diagnostics);

        let e = errors(&run(
            &text.replace("mustUnderstand=\"1\"", "mustUnderstand=\"yes\"")
        ));
        assert_eq!(e.len(), 1, "{e:#?}");
        assert!(
            e[0].starts_with("4:5:") && e[0].contains("must be 0 or 1"),
            "{e:#?}"
        );

        // Any other attribute in the SOAP namespace is still an error.
        let e = errors(&run(&text.replace("soapenv:actor", "soapenv:role")));
        assert!(
            e.iter()
                .any(|m| m.starts_with("4:5:") && m.contains("role")),
            "{e:#?}"
        );
    }

    #[test]
    fn missing_declared_header_is_a_warning() {
        let start = GET_CUSTOMER.find("  <soapenv:Header>").expect("header");
        let end = GET_CUSTOMER.find("  <soapenv:Body>").expect("body");
        let text = format!("{}{}", &GET_CUSTOMER[..start], &GET_CUSTOMER[end..]);
        let v = run(&text);
        assert!(!v.has_errors(), "{:#?}", v.diagnostics);
        let w = warnings(&v);
        assert_eq!(w.len(), 1, "{w:#?}");
        assert!(w[0].contains("RequestContext"), "{w:#?}");
    }

    /// Positions refer to the request text, columns included, with no cutting out.
    #[test]
    fn positions_are_in_the_request_text() {
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
        let e = errors(&run(text));
        assert!(
            e.iter().any(|m| m.starts_with("5:41:")),
            "expected an error at 5:41 (the `<` of `<cus:nope/>`), got {e:#?}"
        );
    }

    #[test]
    fn unknown_body_element_is_an_error_with_a_position() {
        let v = run(&GET_CUSTOMER.replace("GetCustomer", "DeleteCustomer"));
        let e = errors(&v);
        assert!(
            e.iter().any(|m| m.starts_with("7:5: error: no operation")),
            "{e:#?}"
        );
        assert!(v.operation().is_none());
    }

    #[test]
    fn request_bundle_keeps_a_project_supplied_envelope_schema() {
        let mut b = SchemaBundle {
            docs: Vec::new(),
            root: String::new(),
        };
        assert!(request_bundle(&b).get(ENVELOPE_URI).is_some());
        b.docs.push(SchemaDoc {
            uri: "washboard:/wsdl/soap.xsd".into(),
            target_ns: SOAP11_ENV_NS.into(),
            origin: SchemaOrigin::File {
                path: "soap.xsd".into(),
            },
            text: String::new(),
        });
        let r = request_bundle(&b);
        assert!(r.get(ENVELOPE_URI).is_none());
        assert_eq!(r.root, REQUEST_ROOT_URI);
    }
}
