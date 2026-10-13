//! SOAP 1.1 specifics: envelope namespace, fault parsing, envelope construction for templates.

use crate::model::{OperationRef, QName};
use crate::schema::{SchemaError, SchemaModel, TemplateOptions};
use crate::wsdl::{Direction, Support, UnsupportedReason, Wsdl};
use crate::xml;
use crate::xml::escape_attr;
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::fmt::Write as _;
use thiserror::Error;

/// SOAP 1.1 envelope namespace. SOAP 1.2 is not supported.
pub const SOAP11_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// SOAP 1.2 envelope namespace. Only recognized, to tell users that SOAP 1.2 is unsupported.
pub const SOAP12_ENV_NS: &str = "http://www.w3.org/2003/05/soap-envelope";
/// WSDL SOAP 1.1 binding namespace (`soap:binding`, `soap:operation`, `soap:body`, …).
pub const WSDL_SOAP11_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
/// WSDL SOAP 1.2 binding namespace; bindings using it are listed as unsupported.
pub const WSDL_SOAP12_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
pub const WSDL_NS: &str = "http://schemas.xmlsoap.org/wsdl/";
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
pub const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// Prefix of the SOAP envelope namespace in generated requests.
pub const ENVELOPE_PREFIX: &str = "soapenv";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("no operation {} in binding {}", .0.operation, .0.binding)]
    UnknownOperation(OperationRef),
    #[error("operation {} is not supported: {reason}", .operation.operation)]
    Unsupported {
        operation: OperationRef,
        reason: UnsupportedReason,
    },
    #[error("operation {} has no input message", .0.operation)]
    NoInput(OperationRef),
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

/// True if the given Node is a soap:Envelope.
pub fn is_soap11_envelope(element: roxmltree::Node) -> bool {
    element.has_tag_name((SOAP11_ENV_NS, "Envelope"))
}

/// A complete SOAP 1.1 request envelope for an operation's input (`docs/specs/schema-model.md`,
/// "Templates"): the declared `soap:header` blocks in a `Header` (omitted when there are none) and
/// the body element(s) expanded from the schema.
///
/// Namespaces are declared once on the `Envelope` when the templates agree on prefixes, and on
/// each block otherwise. Header parts declared with `type=` have no element to emit and are
/// skipped. The result ends with a newline and has no XML declaration (files are UTF-8).
pub fn request_envelope(
    wsdl: &Wsdl,
    model: &SchemaModel,
    op: &OperationRef,
    opts: &TemplateOptions,
) -> Result<String, EnvelopeError> {
    let (_, operation) = wsdl
        .operation(op)
        .ok_or_else(|| EnvelopeError::UnknownOperation(op.clone()))?;
    if let Support::Unsupported(reason) = &operation.support {
        return Err(EnvelopeError::Unsupported {
            operation: op.clone(),
            reason: reason.clone(),
        });
    }
    let input = operation
        .message(Direction::Input)
        .ok_or_else(|| EnvelopeError::NoInput(op.clone()))?;
    let headers: Vec<QName> = input
        .headers
        .iter()
        .filter_map(|h| h.content.element().cloned())
        .collect();
    let body = operation.body_elements(Direction::Input);

    match blocks(model, &headers, &body, opts, false)? {
        Some((h, b, namespaces)) => Ok(assemble(&h, &b, &namespaces, &opts.indent)),
        None => {
            let (h, b, _) = blocks(model, &headers, &body, opts, true)?
                .unwrap_or_else(|| (Vec::new(), Vec::new(), Vec::new()));
            Ok(assemble(&h, &b, &[], &opts.indent))
        }
    }
}

type Blocks = (Vec<String>, Vec<String>, Vec<(String, String)>);

/// Generates every block. With `local_decls` false, prefixes are shared across blocks and the
/// result is `None` if two blocks bound one prefix to different namespaces.
fn blocks(
    model: &SchemaModel,
    headers: &[QName],
    body: &[QName],
    opts: &TemplateOptions,
    local_decls: bool,
) -> Result<Option<Blocks>, SchemaError> {
    let mut opts = opts.clone();
    opts.declare_namespaces = local_decls;
    opts.prefixes
        .insert(0, (ENVELOPE_PREFIX.to_owned(), SOAP11_ENV_NS.to_owned()));
    let mut bound: HashMap<String, String> = HashMap::new();
    bound.insert(ENVELOPE_PREFIX.to_owned(), SOAP11_ENV_NS.to_owned());
    let mut order: Vec<(String, String)> = Vec::new();
    let mut gen_all = |elements: &[QName]| -> Result<Option<Vec<String>>, SchemaError> {
        let mut out = Vec::new();
        for e in elements {
            let t = model.template(e, &opts)?;
            if !local_decls {
                for (p, ns) in &t.namespaces {
                    match bound.get(p) {
                        Some(existing) if existing != ns => return Ok(None),
                        Some(_) => {}
                        None => {
                            bound.insert(p.clone(), ns.clone());
                            order.push((p.clone(), ns.clone()));
                            opts.prefixes.push((p.clone(), ns.clone()));
                        }
                    }
                }
            }
            out.push(t.xml);
        }
        Ok(Some(out))
    };
    let Some(h) = gen_all(headers)? else {
        return Ok(None);
    };
    let Some(b) = gen_all(body)? else {
        return Ok(None);
    };
    Ok(Some((h, b, order)))
}

/// `indent` is one level, the same as the blocks were generated with.
fn assemble(
    headers: &[String],
    body: &[String],
    namespaces: &[(String, String)],
    indent: &str,
) -> String {
    let p = ENVELOPE_PREFIX;
    let mut s = format!("<{p}:Envelope xmlns:{p}=\"{SOAP11_ENV_NS}\"");
    // Align further declarations under the first, as hand-written envelopes do.
    let pad = " ".repeat(p.len() + "<:Envelope ".len());
    for (prefix, ns) in namespaces {
        write!(s, "\n{pad}xmlns:{prefix}=\"{}\"", escape_attr(ns)).expect("write failed");
    }
    s.push_str(">\n");
    let section = |s: &mut String, name: &str, items: &[String]| {
        writeln!(s, "{indent}<{p}:{name}>").expect("write failed");
        for item in items {
            for line in item.lines() {
                if line.is_empty() {
                    s.push('\n');
                } else {
                    s.push_str(indent);
                    s.push_str(indent);
                    s.push_str(line);
                    s.push('\n');
                }
            }
        }
        writeln!(s, "{indent}</{p}:{name}>").expect("write failed");
    };
    if !headers.is_empty() {
        section(&mut s, "Header", headers);
    }
    section(&mut s, "Body", body);
    writeln!(s, "</{p}:Envelope>").expect("write failed");
    s
}

/// A SOAP 1.1 `Fault` (`soapenv:Body/soapenv:Fault`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoapFault {
    /// `faultcode` text as written, e.g. `soapenv:Server`. Empty if the element is missing.
    pub code: String,
    /// `faultcode` resolved against the namespaces in scope at that element, e.g.
    /// `{http://schemas.xmlsoap.org/soap/envelope/}Server`. `None` if its prefix is unbound.
    pub code_qname: Option<QName>,
    /// `faultstring` text. Empty if the element is missing.
    pub string: String,
    pub actor: Option<String>,
    /// The `detail` element as XML source text (including its own tags), for display as-is;
    /// its content is application-defined.
    pub detail: Option<String>,
}

/// Returns the fault if `body` is a SOAP 1.1 envelope whose `Body` holds a `Fault`.
///
/// Anything else, including HTML error pages, SOAP 1.2 envelopes, undecodable or malformed
/// XML, is `None`: callers then show the body as it is.
pub fn detect_fault(body: &[u8]) -> Option<SoapFault> {
    let decoded = xml::decode(body).ok()?;
    let doc = Document::parse(&decoded.text).ok()?;
    let envelope = doc.root_element();
    if !is_soap11_envelope(envelope) {
        return None;
    }
    let soap_body = envelope.children().find(|n| has_envelope_tag(*n, "Body"))?;
    let fault = soap_body
        .children()
        .find(|n| has_envelope_tag(*n, "Fault"))?;

    // The spec makes these children unqualified; some toolkits qualify them anyway.
    let child = |name: &str| {
        let name_q = roxmltree::ExpandedName::from((SOAP11_ENV_NS, name));
        let name_bare = roxmltree::ExpandedName::from(name);
        fault
            .children()
            .find(|n| [name_q, name_bare].contains(&n.tag_name()))
    };
    let text = |n: Node| n.text().unwrap_or_default().trim().to_owned();

    let code_node = child("faultcode");
    let code = code_node.map(text).unwrap_or_default();
    let code_qname = code_node.and_then(|n| resolve_qname(n, &code));
    Some(SoapFault {
        code,
        code_qname,
        string: child("faultstring").map(text).unwrap_or_default(),
        actor: child("faultactor").map(text),
        detail: child("detail").map(|n| decoded.text[n.range()].to_owned()),
    })
}

fn has_envelope_tag(n: Node, local: &str) -> bool {
    n.has_tag_name((SOAP11_ENV_NS, local))
}

fn resolve_qname(scope: Node, value: &str) -> Option<QName> {
    let (prefix, local) = match value.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, value),
    };
    if local.is_empty() {
        return None;
    }
    let ns = match prefix {
        Some(p) => scope.lookup_namespace_uri(Some(p))?,
        None => scope.lookup_namespace_uri(None).unwrap_or_default(),
    };
    Some(QName::new(ns, local))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{customer, legacy};

    fn with_model(w: Wsdl) -> (Wsdl, SchemaModel) {
        let m = SchemaModel::build(&w.bundle);
        (w, m)
    }

    fn op(ns: &str, binding: &str, name: &str) -> OperationRef {
        OperationRef {
            binding: QName::new(ns, binding),
            operation: name.into(),
        }
    }

    #[test]
    fn customer_envelope_has_header_and_body() {
        let (w, m) = with_model(customer());
        let ns = "urn:example:customer:service";
        let env = request_envelope(
            &w,
            &m,
            &op(ns, "CustomerBinding", "GetCustomer"),
            &TemplateOptions::default(),
        )
        .expect("envelope");
        assert!(env.starts_with("<soapenv:Envelope xmlns:soapenv="), "{env}");
        assert!(env.contains("<soapenv:Header>"), "{env}");
        assert!(env.contains("RequestContext"), "{env}");
        assert!(env.contains("GetCustomer"), "{env}");
        let doc = roxmltree::Document::parse(&env).expect("well-formed");
        assert!(is_soap11_envelope(doc.root_element()));

        let err = request_envelope(
            &w,
            &m,
            &op(ns, "CustomerBinding12", "GetCustomer"),
            &TemplateOptions::default(),
        );
        assert!(matches!(err, Err(EnvelopeError::Unsupported { .. })));
    }

    #[test]
    fn envelope_uses_the_template_indent() {
        let (w, m) = with_model(customer());
        let op = op(
            "urn:example:customer:service",
            "CustomerBinding",
            "GetCustomer",
        );
        for width in [2, 4] {
            let opts = TemplateOptions {
                indent: " ".repeat(width),
                ..TemplateOptions::default()
            };
            let env = request_envelope(&w, &m, &op, &opts).expect("envelope");
            // A new request is already formatted at the same width as Format XML.
            assert_eq!(
                crate::xml::pretty_print(&env, width).as_ref(),
                Ok(&env),
                "width {width}"
            );
        }
    }

    #[test]
    fn rpc_envelope_uses_wrapper() {
        let (w, m) = with_model(legacy());
        let env = request_envelope(
            &w,
            &m,
            &op("urn:example:legacy", "LegacyBinding", "Lookup"),
            &TemplateOptions::default(),
        )
        .expect("envelope");
        assert!(!env.contains("Header"), "{env}");
    }

    const FAULT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/">
  <soapenv:Body>
    <soapenv:Fault>
      <faultcode>soapenv:Client</faultcode>
      <faultstring> Customer 42 not found </faultstring>
      <faultactor>urn:svc</faultactor>
      <detail><e:Err xmlns:e="urn:e">42</e:Err></detail>
    </soapenv:Fault>
  </soapenv:Body>
</soapenv:Envelope>"#;

    #[test]
    fn detects_fault_fields() {
        let f = detect_fault(FAULT.as_bytes()).expect("fault");
        assert_eq!(f.code, "soapenv:Client");
        assert_eq!(f.code_qname, Some(QName::new(SOAP11_ENV_NS, "Client")));
        assert_eq!(f.string, "Customer 42 not found");
        assert_eq!(f.actor.as_deref(), Some("urn:svc"));
        assert_eq!(
            f.detail.as_deref(),
            Some(r#"<detail><e:Err xmlns:e="urn:e">42</e:Err></detail>"#)
        );
    }

    #[test]
    fn cli_fault_example() {
        let response = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><soapenv:Fault><faultcode>soapenv:Server</faultcode><faultstring>customer locked</faultstring></soapenv:Fault></soapenv:Body></soapenv:Envelope>
"#;
        let fault = detect_fault(response.as_bytes()).expect("fault");
        assert_eq!(fault.code, "soapenv:Server");
        assert_eq!(fault.string, "customer locked");
    }

    #[test]
    fn utf16_fault_with_bom() {
        let mut b = vec![0xFF, 0xFE];
        for u in FAULT.replace("UTF-8", "UTF-16").encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert!(detect_fault(&b).is_some());
    }

    #[test]
    fn unbound_prefix_and_qualified_children() {
        let body = r#"<e:Envelope xmlns:e="http://schemas.xmlsoap.org/soap/envelope/"><e:Body>
            <e:Fault><e:faultcode>x:Oops</e:faultcode><e:faultstring>bad</e:faultstring>
            </e:Fault></e:Body></e:Envelope>"#;
        let f = detect_fault(body.as_bytes()).expect("fault");
        assert_eq!(f.code, "x:Oops");
        assert_eq!(f.code_qname, None);
        assert_eq!(f.string, "bad");
        assert_eq!(f.actor, None);
        assert_eq!(f.detail, None);
    }

    #[test]
    fn non_faults() {
        let ok = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
            <r:Resp xmlns:r="urn:r"><r:Fault/></r:Resp></s:Body></s:Envelope>"#;
        assert_eq!(detect_fault(ok.as_bytes()), None);
        let soap12 = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"><s:Body>
            <s:Fault/></s:Body></s:Envelope>"#;
        assert_eq!(detect_fault(soap12.as_bytes()), None);
        assert_eq!(
            detect_fault(b"<html><body>502 Bad Gateway</body></html>"),
            None
        );
        assert_eq!(detect_fault(b"not xml at all"), None);
        assert_eq!(detect_fault(b"\xFF\xFF\xFF"), None);
        assert_eq!(detect_fault(b""), None);
    }
}
