//! What is sent and what came back: input for the response pane, history and the HTTP log.

use std::time::{Duration, SystemTime};

use crate::model::Server;

/// Everything needed to send one SOAP 1.1 request.
#[derive(Debug, Clone)]
pub struct SendRequest {
    pub server: Server,
    /// Basic-auth password, already fetched from the secret store. Ignored for `Auth::None`.
    pub password: Option<String>,
    /// From the binding operation. `None` or empty sends `SOAPAction: ""`.
    pub soap_action: Option<String>,
    /// The envelope exactly as in the editor.
    pub body: String,
}

/// Request or response as shown in the HTTP log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    /// `POST /path HTTP/1.1` or `HTTP/1.1 200 OK`.
    pub start_line: String,
    /// In send order. `Authorization` is stored unmasked; the UI masks it.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TlsInfo {
    /// `None` for plain HTTP.
    pub protocol: Option<String>,
    /// True if the server's "ignore TLS errors" setting was in effect.
    pub verification_skipped: bool,
}

#[derive(Debug, Clone)]
pub struct Exchange {
    pub started_at: SystemTime,
    pub duration: Duration,
    pub request: RawMessage,
    /// `None` if no response was received; then `error` is set.
    pub response: Option<RawMessage>,
    pub tls: TlsInfo,
    pub error: Option<String>,
}
