//! Request validation: well-formedness, SOAP envelope, operation dispatch, XSD via libxml2.
//!
//! Produces [`crate::diag::Diagnostic`]s. Details: `docs/PLAN.md` §4 (validation semantics), §5.

pub mod request;
pub mod xsd;

pub use request::{RequestSchema, Validation, validate_request};
