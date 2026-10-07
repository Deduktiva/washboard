//! The only module allowed to open network connections.
//!
//! It accepts a [`crate::model::Server`] and nothing else as a destination: no redirects, no
//! proxies (environment or system), no other hosts. See `docs/PLAN.md` §6.
//!
//! [`send`] is blocking; callers run it on a short-lived worker thread. Each call builds its own
//! `ureq` agent (native-tls: Security.framework on macOS, OpenSSL elsewhere) so the TLS policy
//! of one server can never leak into a request to another, and no connection is pooled.
//!
//! # What the HTTP log shows
//!
//! Every request header is set explicitly here, so ureq adds none of its own (`Host`,
//! `Content-Length`, `User-Agent` included; `Accept`/`Accept-Encoding` are not sent at all) and
//! [`Exchange::request`] lists exactly the header lines on the wire, in wire order. The body is
//! sent as `Content-Length`-delimited bytes, never chunked or compressed. Header *names* are
//! written in lowercase by ureq (`host: …`), and they are recorded that way; HTTP/1.1 treats
//! them case-insensitively.
//!
//! For the response, ureq does not expose the raw bytes: header names arrive lowercased,
//! repeated headers are grouped by name (relative order of different names is kept), and the
//! reason phrase is not available, so the status line uses the canonical phrase for the code
//! (`HTTP/1.1 500 Internal Server Error` even if the server wrote `500 Error`). The body is the
//! exact bytes received (no decompression, no charset conversion), capped at
//! [`MAX_RESPONSE_BODY`]. native-tls does not report the negotiated TLS version, so
//! [`TlsInfo::protocol`] is just `"TLS"` for HTTPS responses.

pub mod exchange;
mod fault;

use std::io::Read;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use ureq::http::{self, HeaderName, HeaderValue, Uri};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, Timeout};

use crate::model::Auth;

pub use exchange::{Exchange, RawMessage, SendRequest, TlsInfo};
pub use fault::{SoapFault, detect_fault};

/// Response bodies beyond this are truncated and reported as an error. Real SOAP responses are
/// far smaller; this only protects the app from a misbehaving server.
pub const MAX_RESPONSE_BODY: usize = 50 * 1024 * 1024;

/// Used when a server's timeout is zero, so a send can never block a worker forever.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

pub const USER_AGENT: &str = concat!("Washboard/", env!("CARGO_PKG_VERSION"));

pub const CONTENT_TYPE: &str = "text/xml; charset=utf-8";

/// Sends one SOAP 1.1 request and records what went over the wire.
///
/// Never fails: transport problems (bad URL, DNS, connect, TLS, timeout, oversized body) are
/// reported in [`Exchange::error`] with a message meant for the user. Any HTTP status,
/// including 3xx, 4xx and 5xx, is a normal response.
pub fn send(req: &SendRequest) -> Exchange {
    send_with_limit(req, MAX_RESPONSE_BODY)
}

fn send_with_limit(req: &SendRequest, body_limit: usize) -> Exchange {
    let started_at = SystemTime::now();
    let start = Instant::now();
    let body = req.body.as_bytes().to_vec();

    let target = match Target::parse(&req.server.url) {
        Ok(t) => t,
        Err(msg) => {
            let request = RawMessage {
                start_line: format!("POST {} HTTP/1.1", req.server.url),
                headers: Vec::new(),
                body,
            };
            return failed(started_at, start, request, TlsInfo::default(), msg);
        }
    };
    let tls = TlsInfo {
        protocol: None,
        verification_skipped: target.https && req.server.ignore_tls_errors,
    };

    let headers = build_headers(req, &target);
    let request = RawMessage {
        start_line: format!("POST {} HTTP/1.1", target.path_and_query),
        headers: headers
            .iter()
            .map(|(n, v)| (n.as_str().to_owned(), v.clone()))
            .collect(),
        body,
    };
    let mut builder = http::Request::post(target.uri.clone());
    for (name, value) in headers {
        let Ok(value) = HeaderValue::from_str(&value) else {
            let msg = format!(
                "the {name} header contains characters that are not allowed in HTTP headers \
                 (line breaks or control characters)"
            );
            return failed(started_at, start, request, tls, msg);
        };
        builder = builder.header(name, value);
    }
    let http_req = match builder.body(request.body.as_slice()) {
        Ok(r) => r,
        Err(e) => {
            return failed(
                started_at,
                start,
                request,
                tls,
                format!("invalid request: {e}"),
            );
        }
    };

    let timeout = effective_timeout(req.server.timeout);
    let agent = agent(req.server.ignore_tls_errors, timeout);
    let mut response = match agent.run(http_req) {
        Ok(r) => r,
        Err(e) => {
            let msg = describe_error(&e, &target, req.server.ignore_tls_errors, timeout);
            return failed(started_at, start, request, tls, msg);
        }
    };

    let status = response.status();
    let start_line = format!(
        "{:?} {} {}",
        response.version(),
        status.as_u16(),
        status.canonical_reason().unwrap_or_default()
    )
    .trim_end()
    .to_owned();
    let resp_headers = response
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), header_text(v.as_bytes())))
        .collect();

    let mut resp_body = Vec::new();
    let limit = u64::try_from(body_limit)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut error = match response
        .body_mut()
        .as_reader()
        .take(limit)
        .read_to_end(&mut resp_body)
    {
        Ok(_) => None,
        Err(e) => {
            let e = ureq::Error::from(e);
            let msg = describe_error(&e, &target, req.server.ignore_tls_errors, timeout);
            Some(format!("reading the response body failed: {msg}"))
        }
    };
    if resp_body.len() > body_limit {
        resp_body.truncate(body_limit);
        error = Some(format!(
            "the response body is larger than {} MB and was truncated",
            body_limit / (1024 * 1024)
        ));
    }

    Exchange {
        started_at,
        duration: start.elapsed(),
        request,
        response: Some(RawMessage {
            start_line,
            headers: resp_headers,
            body: resp_body,
        }),
        tls: TlsInfo {
            protocol: target.https.then(|| "TLS".to_owned()),
            ..tls
        },
        error,
    }
}

fn failed(
    started_at: SystemTime,
    start: Instant,
    request: RawMessage,
    tls: TlsInfo,
    error: String,
) -> Exchange {
    Exchange {
        started_at,
        duration: start.elapsed(),
        request,
        response: None,
        tls,
        error: Some(error),
    }
}

fn effective_timeout(t: Duration) -> Duration {
    if t.is_zero() { DEFAULT_TIMEOUT } else { t }
}

/// One agent per send: the TLS policy is per server, and nothing is pooled or remembered.
fn agent(ignore_tls_errors: bool, timeout: Duration) -> Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        // The OS trust store, including user-installed corporate CAs. The default would be
        // ureq's bundled Mozilla roots, which would ignore those CAs.
        .root_certs(RootCerts::PlatformVerifier)
        // Disables both chain and hostname verification in native-tls.
        .disable_verification(ignore_tls_errors)
        .build();
    Agent::config_builder()
        .tls_config(tls)
        // The default config reads HTTP(S)_PROXY/ALL_PROXY from the environment.
        .proxy(None)
        // 0 = never follow; the 3xx is returned as the response.
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        // We set every header ourselves; these stop ureq from adding its own.
        .user_agent("")
        .accept("")
        .accept_encoding("")
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .build()
        .new_agent()
}

/// The parts of `server.url` the request needs.
#[derive(Debug)]
struct Target {
    uri: Uri,
    https: bool,
    /// `Host` header value: host, plus the port when it is not the scheme's default.
    host_header: String,
    /// For error messages.
    authority: String,
    path_and_query: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, String> {
        if !url.contains("://") {
            return Err(format!(
                "the server URL \"{url}\" has no http:// or https://"
            ));
        }
        let uri: Uri = url
            .parse()
            .map_err(|e| format!("the server URL \"{url}\" is not a valid URL ({e})"))?;
        let https = match uri.scheme_str().map(str::to_ascii_lowercase).as_deref() {
            Some("https") => true,
            Some("http") => false,
            Some(other) => {
                return Err(format!(
                    "the server URL must start with http:// or https://, not {other}://"
                ));
            }
            None => {
                return Err(format!(
                    "the server URL \"{url}\" has no http:// or https://"
                ));
            }
        };
        let authority = uri
            .authority()
            .ok_or_else(|| format!("the server URL \"{url}\" has no host"))?;
        if authority.as_str().contains('@') {
            // ureq would turn these into an Authorization header we do not log.
            return Err(
                "the server URL contains a user name or password; use the server's \
                        Basic authentication setting instead"
                    .to_owned(),
            );
        }
        let host = authority.host();
        if host.is_empty() {
            return Err(format!("the server URL \"{url}\" has no host"));
        }
        let default_port = if https { 443 } else { 80 };
        let host_header = match authority.port_u16() {
            Some(p) if p != default_port => format!("{host}:{p}"),
            _ => host.to_owned(),
        };
        let path_and_query = match uri.path_and_query().map(|p| p.as_str()) {
            None | Some("") => "/".to_owned(),
            Some(p) if p.starts_with('?') => format!("/{p}"),
            Some(p) => p.to_owned(),
        };
        Ok(Self {
            authority: authority.as_str().to_owned(),
            uri,
            https,
            host_header,
            path_and_query,
        })
    }
}

/// All request headers, in send order. Values are unchecked here; see `send_with_limit`.
fn build_headers(req: &SendRequest, target: &Target) -> Vec<(HeaderName, String)> {
    let mut h = vec![
        (http::header::HOST, target.host_header.clone()),
        (http::header::USER_AGENT, USER_AGENT.to_owned()),
        (http::header::CONTENT_TYPE, CONTENT_TYPE.to_owned()),
        (
            HeaderName::from_static("soapaction"),
            soap_action_value(req.soap_action.as_deref()),
        ),
    ];
    if let Auth::Basic { username } = &req.server.auth {
        let password = req.password.as_deref().unwrap_or_default();
        let creds =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        h.push((http::header::AUTHORIZATION, format!("Basic {creds}")));
    }
    h.push((http::header::CONTENT_LENGTH, req.body.len().to_string()));
    h
}

/// SOAP 1.1 §6.1.1: the value is a quoted URI; `""` means "the intent is the HTTP URI". A value
/// that already carries its own quotes (seen in some WSDLs) is not quoted twice.
fn soap_action_value(action: Option<&str>) -> String {
    match action {
        None | Some("") => "\"\"".to_owned(),
        Some(a) if a.len() >= 2 && a.starts_with('"') && a.ends_with('"') => a.to_owned(),
        Some(a) => format!("\"{a}\""),
    }
}

/// Header values are bytes; show UTF-8 when valid, else Latin-1 (the historical HTTP charset),
/// so no byte is lost.
fn header_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

fn describe_error(e: &ureq::Error, target: &Target, ignore_tls: bool, timeout: Duration) -> String {
    let host = &target.authority;
    let tls_hint = if ignore_tls {
        ""
    } else {
        ". If the server uses a self-signed or internal certificate, install its CA in the \
         system keychain or enable \"Ignore TLS errors\" for this server"
    };
    match e {
        ureq::Error::HostNotFound => format!("could not resolve host name of {host}"),
        ureq::Error::Timeout(t) => {
            let phase = match t {
                Timeout::Resolve => " while resolving the host name",
                Timeout::Connect => " while connecting",
                Timeout::SendRequest | Timeout::SendBody => " while sending the request",
                Timeout::RecvResponse => " while waiting for the response",
                Timeout::RecvBody => " while receiving the response body",
                _ => "",
            };
            format!("timed out after {}{phase} ({host})", fmt_duration(timeout))
        }
        ureq::Error::Tls(msg) => format!("TLS error with {host}: {msg}{tls_hint}"),
        ureq::Error::NativeTls(err) => {
            format!("TLS handshake with {host} failed: {err}{tls_hint}")
        }
        ureq::Error::ConnectionFailed => format!("could not connect to {host}"),
        ureq::Error::Io(io) => {
            use std::io::ErrorKind as K;
            match io.kind() {
                K::ConnectionRefused => format!("connection refused by {host}"),
                K::TimedOut => format!("timed out after {} ({host})", fmt_duration(timeout)),
                K::ConnectionReset | K::ConnectionAborted => {
                    format!("connection to {host} was reset by the server")
                }
                K::UnexpectedEof => format!("{host} closed the connection unexpectedly"),
                K::HostUnreachable | K::NetworkUnreachable => {
                    format!("{host} is unreachable ({io})")
                }
                _ => format!("network error with {host}: {io}"),
            }
        }
        ureq::Error::Protocol(p) => format!("{host} sent an invalid HTTP response: {p}"),
        ureq::Error::LargeResponseHeader(..) => {
            format!("the response headers from {host} are too large")
        }
        other => format!("request to {host} failed: {other}"),
    }
}

fn fmt_duration(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

#[cfg(test)]
mod tests;
