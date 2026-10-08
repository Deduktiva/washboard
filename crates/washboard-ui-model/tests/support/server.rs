//! A one-shot HTTP server on 127.0.0.1 for the model's send tests.
//!
//! It lives under `tests/` because only `washboard_core::http` may use sockets in shipped code
//! (PLAN §6, enforced by `washboard-core/tests/network_boundary.rs`); the unit tests include
//! it with `#[path]`, so it is compiled for tests only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Duration;

/// Accepts one connection on 127.0.0.1, waits `delay`, answers with `status` and `body`.
/// The handle yields the raw request.
pub fn serve_once(
    status: &'static str,
    body: &'static str,
    delay: Duration,
) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://127.0.0.1:{}/soap",
        listener.local_addr().expect("addr").port()
    );
    let handle = std::thread::spawn(move || {
        let (mut tcp, _) = listener.accept().expect("accept");
        tcp.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut reader = BufReader::new(tcp.try_clone().expect("clone"));
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().expect("length");
            }
            head.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let mut request_body = vec![0; length];
        reader.read_exact(&mut request_body).expect("body");
        std::thread::sleep(delay);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        // The client may have given up already.
        let _ = tcp.write_all(response.as_bytes());
        head + &String::from_utf8_lossy(&request_body)
    });
    (url, handle)
}

/// A URL on a port nobody listens on any more.
pub fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    format!(
        "http://127.0.0.1:{}/",
        listener.local_addr().expect("addr").port()
    )
}
