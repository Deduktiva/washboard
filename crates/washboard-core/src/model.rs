//! Domain types shared across modules and with the UI.

use std::fmt;
use std::time::{Duration, SystemTime};

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

/// Identifies an operation within a WSDL: binding QName + operation name.
///
/// Bindings, not ports, define SOAP details; several ports may share one binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationRef {
    pub binding: QName,
    pub operation: String,
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
