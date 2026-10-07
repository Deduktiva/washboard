//! Tests against local servers on 127.0.0.1 only; nothing here touches the outside network.
//!
//! The server is hand-written on `TcpListener` so it can hand back the raw request bytes,
//! which is what proves that the log matches the wire.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;
use crate::model::{Server, ServerId};

const ENVELOPE: &str = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><a xmlns="urn:a">ä</a></soapenv:Body></soapenv:Envelope>"#;

fn server(url: String) -> Server {
    Server {
        id: ServerId::new(),
        name: "test".into(),
        url,
        ignore_tls_errors: false,
        auth: Auth::None,
        timeout: Duration::from_secs(10),
    }
}

fn request(server: Server) -> SendRequest {
    SendRequest {
        server,
        password: None,
        soap_action: Some("urn:a#Do".into()),
        body: ENVELOPE.into(),
    }
}

/// What the server does with the one connection it accepts.
struct Script {
    response: Vec<u8>,
    delay: Duration,
    tls: Option<Arc<rustls::ServerConfig>>,
}

impl Script {
    fn plain(response: impl Into<Vec<u8>>) -> Self {
        Self {
            response: response.into(),
            delay: Duration::ZERO,
            tls: None,
        }
    }
}

/// Accepts one connection, reads one request, answers with the script. Returns the port and a
/// handle yielding the raw request bytes (empty if the client gave up, e.g. on a TLS error).
fn serve_once(script: Script) -> (u16, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept");
        tcp.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        match script.tls {
            // The handshake runs on first read; if the client rejects the certificate, that
            // read fails and `exchange` returns the empty request.
            Some(config) => match rustls::ServerConnection::new(config) {
                Ok(conn) => exchange(
                    rustls::StreamOwned::new(conn, tcp),
                    &script.response,
                    script.delay,
                ),
                Err(_) => Vec::new(),
            },
            None => exchange(tcp, &script.response, script.delay),
        }
    });
    (port, handle)
}

fn exchange<S: Read + Write>(stream: S, response: &[u8], delay: Duration) -> Vec<u8> {
    let mut reader = BufReader::new(stream);
    let mut raw = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
            return raw;
        }
        raw.extend_from_slice(&line);
        let text = String::from_utf8_lossy(&line).to_ascii_lowercase();
        if let Some(v) = text.strip_prefix("content-length:") {
            content_length = v.trim().parse().expect("content-length");
        }
        if line == b"\r\n" {
            break;
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("body");
    raw.extend_from_slice(&body);
    thread::sleep(delay);
    let stream = reader.get_mut();
    let _ = stream.write_all(response);
    let _ = stream.flush();
    raw
}

/// Rebuilds the request bytes from the log, the way HTTP/1.1 puts them on the wire.
fn wire_from_log(m: &RawMessage) -> Vec<u8> {
    let mut out = format!("{}\r\n", m.start_line);
    for (n, v) in &m.headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(&m.body);
    out
}

fn header<'a>(m: &'a RawMessage, name: &str) -> Option<&'a str> {
    m.headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn ok_response(body: &[u8]) -> Vec<u8> {
    let mut r = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nX-Multi: 1\r\n\
         Content-Length: {}\r\nX-Multi: 2\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body);
    r
}

/// TLS for the test server comes from rustls, not native-tls: Security.framework refuses to
/// import rcgen's PKCS#8 key as an identity (errSecUnknownFormat), and the server side is not
/// what these tests are about. TLS 1.2 stays enabled because the client's Security.framework
/// backend does not speak TLS 1.3.
fn self_signed() -> Arc<rustls::ServerConfig> {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("cert");
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(vec![ck.cert.der().clone()], key.into())
    .expect("server config");
    Arc::new(config)
}

#[test]
fn log_matches_wire_exactly_with_preemptive_basic_auth() {
    let (port, handle) = serve_once(Script::plain(ok_response(b"<ok/>")));
    let mut s = server(format!("http://127.0.0.1:{port}/ws/customer?wsdl=no"));
    s.auth = Auth::Basic {
        username: "alice".into(),
    };
    let mut req = request(s);
    req.password = Some("s3cr:t".into());

    let ex = send(&req);
    let wire = handle.join().expect("server");

    assert_eq!(ex.error, None);
    assert_eq!(
        String::from_utf8_lossy(&wire),
        String::from_utf8_lossy(&wire_from_log(&ex.request))
    );
    assert_eq!(ex.request.start_line, "POST /ws/customer?wsdl=no HTTP/1.1");
    assert_eq!(
        header(&ex.request, "host"),
        Some(&*format!("127.0.0.1:{port}"))
    );
    assert_eq!(
        header(&ex.request, "content-type"),
        Some("text/xml; charset=utf-8")
    );
    assert_eq!(header(&ex.request, "soapaction"), Some("\"urn:a#Do\""));
    // alice:s3cr:t
    assert_eq!(
        header(&ex.request, "authorization"),
        Some("Basic YWxpY2U6czNjcjp0")
    );
    assert_eq!(
        header(&ex.request, "content-length"),
        Some(&*ENVELOPE.len().to_string())
    );
    assert_eq!(ex.request.body, ENVELOPE.as_bytes());

    let resp = ex.response.expect("response");
    assert_eq!(resp.start_line, "HTTP/1.1 200 OK");
    assert_eq!(resp.body, b"<ok/>");
    assert_eq!(
        header(&resp, "content-type"),
        Some("text/xml; charset=utf-8")
    );
    let multi: Vec<_> = resp
        .headers
        .iter()
        .filter(|(n, _)| n == "x-multi")
        .collect();
    assert_eq!(multi.len(), 2);
    assert_eq!(ex.tls, TlsInfo::default());
}

#[test]
fn soap_action_quoting() {
    assert_eq!(soap_action_value(None), "\"\"");
    assert_eq!(soap_action_value(Some("")), "\"\"");
    assert_eq!(soap_action_value(Some("urn:x")), "\"urn:x\"");
    assert_eq!(soap_action_value(Some("\"urn:x\"")), "\"urn:x\"");
    assert_eq!(soap_action_value(Some("\"")), "\"\"\"");

    for action in [None, Some(String::new())] {
        let (port, handle) = serve_once(Script::plain(ok_response(b"")));
        let mut req = request(server(format!("http://127.0.0.1:{port}")));
        req.soap_action = action;
        let ex = send(&req);
        let wire = String::from_utf8(handle.join().expect("server")).expect("utf8");
        assert_eq!(ex.error, None);
        assert!(wire.contains("\r\nsoapaction: \"\"\r\n"), "{wire}");
        assert!(wire.starts_with("POST / HTTP/1.1\r\n"), "{wire}");
        assert_eq!(header(&ex.request, "authorization"), None);
    }
}

#[test]
fn header_injection_is_refused_without_connecting() {
    let mut req = request(server("http://127.0.0.1:9/".into()));
    req.soap_action = Some("urn:a\r\nX-Evil: 1".into());
    let ex = send(&req);
    assert!(ex.response.is_none());
    assert!(
        ex.error.as_deref().unwrap_or("").contains("soapaction"),
        "{:?}",
        ex.error
    );
}

#[test]
fn redirect_is_returned_not_followed() {
    let other = TcpListener::bind("127.0.0.1:0").expect("bind");
    let other_port = other.local_addr().expect("addr").port();
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{other_port}/elsewhere\r\n\
         Content-Length: 0\r\n\r\n"
    );
    let (port, handle) = serve_once(Script::plain(response));
    let ex = send(&request(server(format!("http://127.0.0.1:{port}/ws"))));
    handle.join().expect("server");

    assert_eq!(ex.error, None);
    let resp = ex.response.expect("response");
    assert_eq!(resp.start_line, "HTTP/1.1 302 Found");
    assert!(header(&resp, "location").is_some());
    other.set_nonblocking(true).expect("nonblocking");
    assert!(other.accept().is_err(), "redirect target was contacted");
}

#[test]
fn timeout_is_reported() {
    let (port, handle) = serve_once(Script {
        delay: Duration::from_secs(3),
        ..Script::plain(ok_response(b"late"))
    });
    let mut s = server(format!("http://127.0.0.1:{port}"));
    s.timeout = Duration::from_millis(300);
    let ex = send(&request(s));
    assert!(ex.response.is_none());
    let err = ex.error.expect("error");
    assert!(err.contains("timed out after 300 ms"), "{err}");
    assert!(ex.duration < Duration::from_secs(2), "{:?}", ex.duration);
    handle.join().expect("server");
}

#[test]
fn connection_refused_is_reported() {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let ex = send(&request(server(format!("http://127.0.0.1:{port}"))));
    assert!(ex.response.is_none());
    let err = ex.error.expect("error");
    assert!(err.contains("refused") || err.contains("connect"), "{err}");
    // The request is still logged as it would have been sent.
    assert_eq!(
        header(&ex.request, "host"),
        Some(&*format!("127.0.0.1:{port}"))
    );
}

#[test]
fn bad_urls_are_rejected_before_connecting() {
    for (url, needle) in [
        ("ftp://example.invalid/x", "http:// or https://"),
        ("example.invalid/x", "http:// or https://"),
        ("http://user:pw@127.0.0.1:9/", "user name or password"),
        ("http://exa mple/", "not a valid URL"),
    ] {
        let ex = send(&request(server(url.into())));
        assert!(ex.response.is_none());
        let err = ex.error.expect("error");
        assert!(err.contains(needle), "{url}: {err}");
    }
}

#[test]
fn host_header_omits_default_port() {
    let t = Target::parse("https://Example.org:443").expect("parse");
    assert_eq!(t.host_header, "Example.org");
    assert_eq!(t.path_and_query, "/");
    assert!(t.https);
    let t = Target::parse("http://[::1]:8080/a?b").expect("parse");
    assert_eq!(t.host_header, "[::1]:8080");
    assert_eq!(t.path_and_query, "/a?b");
}

#[test]
fn agent_ignores_environment_proxies_and_redirects() {
    let a = agent(false, Duration::from_secs(1));
    assert!(a.config().proxy().is_none());
    assert_eq!(a.config().max_redirects(), 0);
    assert!(!a.config().http_status_as_error());
    assert!(!a.config().tls_config().disable_verification());
    assert!(
        agent(true, Duration::from_secs(1))
            .config()
            .tls_config()
            .disable_verification()
    );
}

#[test]
fn https_self_signed_fails_with_verification() {
    let (port, handle) = serve_once(Script {
        tls: Some(self_signed()),
        ..Script::plain(ok_response(b"<ok/>"))
    });
    let ex = send(&request(server(format!("https://localhost:{port}/ws"))));
    handle.join().expect("server");
    assert!(ex.response.is_none());
    let err = ex.error.expect("error");
    assert!(err.contains("TLS"), "{err}");
    assert!(err.contains("Ignore TLS errors"), "{err}");
    assert!(!ex.tls.verification_skipped);
    assert_eq!(ex.tls.protocol, None);
}

#[test]
fn https_self_signed_succeeds_when_ignoring_tls_errors() {
    let (port, handle) = serve_once(Script {
        tls: Some(self_signed()),
        ..Script::plain(ok_response(b"<ok/>"))
    });
    // 127.0.0.1 is not in the certificate either: hostname checks are off too.
    let mut s = server(format!("https://127.0.0.1:{port}/ws"));
    s.ignore_tls_errors = true;
    let ex = send(&request(s));
    let wire = handle.join().expect("server");
    assert_eq!(ex.error, None);
    assert_eq!(wire, wire_from_log(&ex.request));
    assert_eq!(ex.response.expect("response").body, b"<ok/>");
    assert!(ex.tls.verification_skipped);
    assert_eq!(ex.tls.protocol.as_deref(), Some("TLS"));
}

#[test]
fn fault_response_is_a_response_and_detectable() {
    let fault = br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>s:Server</faultcode><faultstring>boom</faultstring></s:Fault></s:Body></s:Envelope>"#;
    let mut response = format!(
        "HTTP/1.1 500 Whatever\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\r\n",
        fault.len()
    )
    .into_bytes();
    response.extend_from_slice(fault);
    let (port, handle) = serve_once(Script::plain(response));
    let ex = send(&request(server(format!("http://127.0.0.1:{port}"))));
    handle.join().expect("server");
    assert_eq!(ex.error, None);
    let resp = ex.response.expect("response");
    // The reason phrase is not exposed by ureq; the canonical one is used.
    assert_eq!(resp.start_line, "HTTP/1.1 500 Internal Server Error");
    let f = detect_fault(&resp.body).expect("fault");
    assert_eq!(f.string, "boom");
    assert_eq!(
        f.code_qname,
        Some(crate::model::QName::new(
            crate::soap::SOAP11_ENV_NS,
            "Server"
        ))
    );
}

#[test]
fn non_utf8_body_bytes_are_preserved() {
    let body: &[u8] = b"<?xml version='1.0' encoding='ISO-8859-1'?><a>\xE4\xFF\x00\xFE</a>";
    // Close-delimited body (no Content-Length) on purpose.
    let mut response =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nConnection: close\r\n\r\n".to_vec();
    response.extend_from_slice(body);
    let (port, handle) = serve_once(Script::plain(response));
    let ex = send(&request(server(format!("http://127.0.0.1:{port}"))));
    handle.join().expect("server");
    assert_eq!(ex.error, None);
    assert_eq!(ex.response.expect("response").body, body);
}

#[test]
fn oversized_body_is_truncated_with_error() {
    let body = vec![b'x'; 3 * 1024 * 1024];
    let (port, handle) = serve_once(Script::plain(ok_response(&body)));
    let ex = send_with_limit(
        &request(server(format!("http://127.0.0.1:{port}"))),
        2 * 1024 * 1024,
    );
    let _ = handle.join();
    let resp = ex.response.expect("response");
    assert_eq!(resp.body.len(), 2 * 1024 * 1024);
    let err = ex.error.expect("error");
    assert!(err.contains("larger than 2 MB"), "{err}");
}

#[test]
fn chunked_response_is_dechunked() {
    let response =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n<a>\r\n4\r\n</a>\r\n0\r\n\r\n";
    let (port, handle) = serve_once(Script::plain(response.to_vec()));
    let ex = send(&request(server(format!("http://127.0.0.1:{port}"))));
    handle.join().expect("server");
    assert_eq!(ex.error, None);
    assert_eq!(ex.response.expect("response").body, b"<a></a>");
}
