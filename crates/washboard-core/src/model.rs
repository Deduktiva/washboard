//! Domain types shared across modules and with the UI.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use thiserror::Error;
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Stored in the project database; stable across folder moves. Keychain key prefix.
    ProjectId
);
id_type!(ServerId);
id_type!(
    /// Stable across renames; history is keyed by this, not by the request name.
    RequestId
);
id_type!(HistoryId);

/// An XML qualified name. `ns` is the namespace URI, empty for no namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QName {
    pub ns: String,
    pub local: String,
}

impl QName {
    pub fn new(ns: impl Into<String>, local: impl Into<String>) -> Self {
        Self {
            ns: ns.into(),
            local: local.into(),
        }
    }
}

impl fmt::Display for QName {
    /// Clark notation: `{ns}local`, or `local` without a namespace.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ns.is_empty() {
            f.write_str(&self.local)
        } else {
            write!(f, "{{{}}}{}", self.ns, self.local)
        }
    }
}

/// Text that is not a [`QName`] or [`OperationRef`] in the notation their `Display` writes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0:?} is not of the form {{namespace}}name")]
pub struct ParseNameError(pub String);

impl FromStr for QName {
    type Err = ParseNameError;

    /// Parses Clark notation as `Display` writes it. Text without a leading `{` is a name in no
    /// namespace. The local part must not be empty, and an opening `{` needs its `}`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ns, local) = match s.strip_prefix('{') {
            Some(rest) => rest
                .split_once('}')
                .ok_or_else(|| ParseNameError(s.to_owned()))?,
            None => ("", s),
        };
        if local.is_empty() {
            return Err(ParseNameError(s.to_owned()));
        }
        Ok(QName::new(ns, local))
    }
}

/// Identifies an operation within a WSDL: binding QName + operation name.
///
/// Bindings, not ports, define SOAP details; several ports may share one binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationRef {
    pub binding: QName,
    pub operation: String,
}

impl fmt::Display for OperationRef {
    /// `{ns}Binding#Operation`: the form stored as a request's operation hint and accepted by
    /// the command-line tool.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.binding, self.operation)
    }
}

impl FromStr for OperationRef {
    type Err = ParseNameError;

    /// The inverse of `Display`. The last `#` separates the operation, so namespaces may
    /// contain `#` (operation names, being NCNames, cannot).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseNameError(s.to_owned());
        let (binding, operation) = s.rsplit_once('#').ok_or_else(err)?;
        if operation.is_empty() {
            return Err(err());
        }
        Ok(OperationRef {
            binding: binding.parse().map_err(|_| err())?,
            operation: operation.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    None,
    /// Sent preemptively. The password lives in the [`crate::secrets::SecretStore`].
    Basic {
        username: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub id: ServerId,
    pub name: String,
    pub url: String,
    /// Disables certificate chain and hostname verification for this server only.
    pub ignore_tls_errors: bool,
    pub auth: Auth,
    pub timeout: Duration,
}

/// Metadata of a request. The XML itself lives in `requests/<name>.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMeta {
    pub id: RequestId,
    /// File stem; also the display name.
    pub name: String,
    /// The operation the request was created for. A hint only; may go stale after a WSDL
    /// replacement. Dispatch at send time uses the Body content.
    pub operation: Option<OperationRef>,
    pub last_server: Option<ServerId>,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: HistoryId,
    pub request_id: RequestId,
    pub server_id: Option<ServerId>,
    pub url: String,
    pub sent_at: SystemTime,
    pub duration: Option<Duration>,
    pub http_status: Option<u16>,
    pub soap_fault: bool,
    /// Transport error (connect, TLS, timeout). `None` when a response was received.
    pub error: Option<String>,
}

/// Where a schema document came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaOrigin {
    /// An `.xsd` file (or a WSDL used as XSD), path relative to the project's `wsdl/` folder.
    File { path: String },
    /// The `index`-th `xs:schema` inside `wsdl:types` of a WSDL file, with the WSDL's in-scope
    /// namespace declarations already copied onto it.
    InlineWsdl { wsdl_path: String, index: usize },
    /// Generated: rpc/literal wrapper elements, per-namespace include wrappers, or the root.
    Generated,
}

/// One schema document, already decoded (see [`crate::xml::decode`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDoc {
    /// The URI other documents use to reference this one, and the only thing the libxml2
    /// resource loader resolves. Files: `washboard:/wsdl/<project-relative path>`
    /// (percent-encoded, see `wsdl::file_uri`). Inline/generated:
    /// `washboard:/inline/<n>.xsd`, `washboard:/rpc/<n>.xsd`, `washboard:/ns/<n>.xsd`,
    /// `washboard:/root.xsd`. All `schemaLocation`s inside `text` are rewritten to these URIs.
    /// They are absolute on purpose: libxml2 resolves a `schemaLocation` against the
    /// including document's URI before asking the loader, which would mangle relative paths.
    /// The loader should therefore look up the URI it is given verbatim.
    pub uri: String,
    /// Empty for no-namespace (chameleon) schemas.
    pub target_ns: String,
    pub origin: SchemaOrigin,
    pub text: String,
}

/// The complete, closed set of schema documents of a project.
///
/// Produced by `wsdl` (WP-WSDL); consumed by `validate`/libxml2 (WP-LIBXML2) and by the Rust
/// schema model (WP-SCHEMA). Nothing outside the bundle may be loaded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SchemaBundle {
    pub docs: Vec<SchemaDoc>,
    /// URI of the generated root schema that imports every namespace exactly once.
    pub root: String,
}

impl SchemaBundle {
    pub fn get(&self, uri: &str) -> Option<&SchemaDoc> {
        self.docs.iter().find(|d| d.uri == uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qname_text_round_trips() {
        for q in [QName::new("urn:x", "a"), QName::new("", "b")] {
            assert_eq!(q.to_string().parse::<QName>(), Ok(q));
        }
        assert_eq!("{}c".parse::<QName>(), Ok(QName::new("", "c")));
        for bad in ["", "{urn:x}", "{urn:x"] {
            assert!(bad.parse::<QName>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn operation_text_round_trips() {
        let op = OperationRef {
            binding: QName::new("urn:x#frag", "Binding"),
            operation: "GetCustomer".into(),
        };
        assert_eq!(op.to_string(), "{urn:x#frag}Binding#GetCustomer");
        assert_eq!(op.to_string().parse::<OperationRef>(), Ok(op));
        let local = OperationRef {
            binding: QName::new("", "B"),
            operation: "Op".into(),
        };
        assert_eq!(local.to_string().parse::<OperationRef>(), Ok(local));
        for bad in ["garbage", "{urn:x#Op", "B#", "#Op"] {
            assert!(bad.parse::<OperationRef>().is_err(), "{bad:?}");
        }
    }
}
