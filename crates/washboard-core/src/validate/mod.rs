//! Request validation: well-formedness, SOAP envelope, operation dispatch, XSD via libxml2.
//!
//! Produces [`crate::diag::Diagnostic`]s. Details: `docs/specs/validation.md`.

pub mod request;
pub mod xsd;

pub use request::{
    RequestSchema, ResolvedValidation, Validation, validate_request, validate_with_parameters,
};
