//! A local HTTP server on 127.0.0.1 for the send tests of the UI model, the CLI and the app.
//!
//! Core's own HTTP tests keep their server, which also speaks TLS and answers with scripted raw
//! bytes.

#![allow(clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// What the server saw of one request.
#[derive(Debug)]
pub struct Seen {
    /// The request line and headers, each line ending in CRLF, without the blank line.
    pub head: String,
    pub body: String,
}

/// Accepts one connection, waits `delay`, answers with `status` (`"200 OK"`) and `body`. The
/// handle yields the raw request.
pub fn serve_once(
    status: &'static str,
    body: &'static str,
    delay: Duration,
) -> (String, JoinHandle<String>) {
    let (listener, url) = bind();
    let handle = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept");
        let seen = exchange(tcp, status, body, delay);
        format!("{}\r\n{}", seen.head, seen.body)
    });
    (url, handle)
}

/// Answers `responses` in order, one per connection, then stops. The receiver gets each
/// request as it is answered.
pub fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, mpsc::Receiver<Seen>) {
    let (listener, url) = bind();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let (tcp, _) = listener.accept().expect("accept");
            let _ = tx.send(exchange(tcp, status, body, Duration::ZERO));
        }
    });
    (url, rx)
}

/// A URL on a port nobody listens on any more.
pub fn closed_port_url() -> String {
    bind().1
}

fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://127.0.0.1:{}/soap",
        listener.local_addr().expect("addr").port()
    );
    (listener, url)
}

/// Reads one request (its head and, with a `Content-Length`, its body), waits `delay` and
/// answers. The client may have given up by then, also before sending all of it; what arrived
/// is returned.
fn exchange(tcp: TcpStream, status: &str, body: &str, delay: Duration) -> Seen {
    tcp.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let mut reader = BufReader::new(tcp);
    let mut head = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().expect("length");
        }
        head.push_str(&line);
    }
    let mut request_body = Vec::with_capacity(length);
    let _ = reader
        .by_ref()
        .take(length as u64)
        .read_to_end(&mut request_body);
    std::thread::sleep(delay);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = reader.get_mut().write_all(response.as_bytes());
    Seen {
        head,
        body: String::from_utf8_lossy(&request_body).into_owned(),
    }
}
