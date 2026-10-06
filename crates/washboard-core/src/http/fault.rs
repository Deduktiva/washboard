//! SOAP 1.1 fault detection on a response body.
//!
//! Lives next to `send` because it inspects what came back; the response pane uses it to call
//! out faults and history uses it for `HistoryEntry::soap_fault`. Faults are recognised by
//! structure, not by HTTP status: SOAP 1.1 says 500, but servers also send faults with 200.

use roxmltree::{Document, Node};

use crate::model::QName;
use crate::soap::SOAP11_ENV_NS;
use crate::xml;

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
    if !is_env(envelope, "Envelope") {
        return None;
    }
    let soap_body = envelope.children().find(|n| is_env(*n, "Body"))?;
    let fault = soap_body
        .children()
        .find(|n| n.is_element())
        .filter(|n| is_env(*n, "Fault"))?;

    // The spec makes these children unqualified; some toolkits qualify them anyway.
    let child = |name: &str| {
        fault.children().find(|n| {
            n.is_element()
                && n.tag_name().name() == name
                && matches!(n.tag_name().namespace(), None | Some(SOAP11_ENV_NS))
        })
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

fn is_env(n: Node, local: &str) -> bool {
    n.is_element()
        && n.tag_name().name() == local
        && n.tag_name().namespace() == Some(SOAP11_ENV_NS)
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
