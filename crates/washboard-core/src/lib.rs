//! Washboard core: everything except the UI.
//!
//! This crate must build and test on Linux. No AppKit, no objc. Platform-specific
//! pieces (Keychain) sit behind traits with a `cfg(target_os = "macos")` implementation.
//!
//! The types in [`model`], [`diag`] and [`http::exchange`] are shared contracts between
//! work packages (see `docs/TASKS.md`). Change them deliberately and update all users.

pub mod diag;
pub mod http;
pub mod model;
pub mod project;
pub mod schema;
pub mod secrets;
pub mod soap;
pub mod validate;
pub mod wsdl;
pub mod xml;

#[cfg(test)]
mod test_support;
