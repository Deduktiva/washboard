//! SOAP 1.1 specifics: envelope namespace, fault parsing, envelope construction for templates.

use std::collections::HashMap;

use thiserror::Error;

use crate::model::{OperationRef, QName};
use crate::schema::{SchemaError, SchemaModel, TemplateOptions};
use crate::wsdl::{Direction, PartContent, Support, UnsupportedReason, Wsdl};
use crate::xml::escape_attr;

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

/// A complete SOAP 1.1 request envelope for an operation's input (PLAN "Template
/// generation"): the declared `soap:header` blocks in a `Header` (omitted when there are none)
/// and the body element(s) expanded from the schema.
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
        .filter_map(|h| match &h.content {
            PartContent::Element(q) => Some(q.clone()),
            _ => None,
        })
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
        s.push_str(&format!("\n{pad}xmlns:{prefix}=\"{}\"", escape_attr(ns)));
    }
    s.push_str(">\n");
    let section = |s: &mut String, name: &str, items: &[String]| {
        s.push_str(&format!("{indent}<{p}:{name}>\n"));
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
        s.push_str(&format!("{indent}</{p}:{name}>\n"));
    };
    if !headers.is_empty() {
        section(&mut s, "Header", headers);
    }
    section(&mut s, "Body", body);
    s.push_str(&format!("</{p}:Envelope>\n"));
    s
}

/// QNames of the element children of a SOAP 1.1 `Envelope`'s `Body`, in document order.
///
/// `None` when `text` is not well-formed or not a SOAP 1.1 envelope with a `Body`. Used to pick
/// the operation (and its `SOAPAction`) at send time; validation reports the details.
pub fn body_elements(text: &str) -> Option<Vec<QName>> {
    let doc = roxmltree::Document::parse(text).ok()?;
    let env = doc.root_element();
    let in_env = |n: &roxmltree::Node<'_, '_>, local: &str| {
        n.tag_name().namespace() == Some(SOAP11_ENV_NS) && n.tag_name().name() == local
    };
    if !in_env(&env, "Envelope") {
        return None;
    }
    let body = env
        .children()
        .filter(|n| n.is_element())
        .find(|n| in_env(n, "Body"))?;
    Some(
        body.children()
            .filter(|n| n.is_element())
            .map(|n| {
                QName::new(
                    n.tag_name().namespace().unwrap_or_default(),
                    n.tag_name().name(),
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::test_support::fixtures;
    use crate::wsdl::{self, Sources};

    fn load(dir: &str, entry: &str, extra: &[&str]) -> (Wsdl, SchemaModel) {
        let root = fixtures().join(dir);
        let extra: Vec<PathBuf> = extra.iter().map(|e| root.join(e)).collect();
        let w = wsdl::load(&Sources::from_disk(&root.join(entry), &extra).expect("sources"));
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
        let (w, m) = load(
            "customer",
            "CustomerService.wsdl",
            &["CustomerBinding.wsdl", "xsd"],
        );
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
        assert_eq!(
            body_elements(&env),
            Some(vec![QName::new(
                "urn:example:customer:messages",
                "GetCustomer"
            )])
        );
        roxmltree::Document::parse(&env).expect("well-formed");

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
        let (w, m) = load(
            "customer",
            "CustomerService.wsdl",
            &["CustomerBinding.wsdl", "xsd"],
        );
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
        let (w, m) = load("legacy-rpc", "Legacy.wsdl", &[]);
        let env = request_envelope(
            &w,
            &m,
            &op("urn:example:legacy", "LegacyBinding", "Lookup"),
            &TemplateOptions::default(),
        )
        .expect("envelope");
        assert!(!env.contains("Header"), "{env}");
        assert_eq!(
            body_elements(&env),
            Some(vec![QName::new("urn:example:legacy", "Lookup")])
        );
    }

    #[test]
    fn body_elements_rejects_non_envelopes() {
        assert_eq!(body_elements("<a/>"), None);
        assert_eq!(body_elements("<a"), None);
        let soap12 =
            format!(r#"<s:Envelope xmlns:s="{SOAP12_ENV_NS}"><s:Body><x/></s:Body></s:Envelope>"#);
        assert_eq!(body_elements(&soap12), None);
    }
}
