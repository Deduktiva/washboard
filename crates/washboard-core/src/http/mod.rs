//! The only module allowed to open network connections.
//!
//! It accepts a [`crate::model::Server`] and nothing else as a destination: no redirects, no
//! proxies (environment or system), no other hosts. See `docs/PLAN.md` §6.
//!
//! Planned additions (WP-HTTP): blocking `send(&SendRequest) -> Exchange` on `ureq` with
//! `native-tls`, per-server TLS policy, preemptive basic auth, timeouts.

pub mod exchange;

pub use exchange::{Exchange, RawMessage, SendRequest, TlsInfo};
