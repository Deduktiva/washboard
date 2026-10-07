//! Request validation: `washboard_core::validate`'s pipeline, and the one place `send` goes
//! through before it sends anything.

use std::path::Path;
use std::process::ExitCode;

use anyhow::bail;
use washboard_core::diag::Severity;
use washboard_core::model::RequestMeta;
use washboard_core::validate::{self, RequestSchema, Validation};
use washboard_core::wsdl::Wsdl;

use crate::support;

/// Validates `text` (the request's content) against the project's WSDL and prints every
/// diagnostic, warnings included, to stderr. Whether the request may be sent is
/// [`Validation::has_errors`]; the `Err` case means the project itself is broken.
///
/// The schema is compiled per call: the CLI is one command per process, where the app
/// compiles once in the background when the project opens.
pub fn check(wsdl: &Wsdl, request: &RequestMeta, text: &str) -> anyhow::Result<Validation> {
    let schema = match RequestSchema::compile(&wsdl.bundle) {
        Ok(s) => s,
        Err(diagnostics) => {
            for d in &diagnostics {
                eprintln!("{d}");
            }
            bail!("the project's schemas do not compile");
        }
    };
    let result = validate::validate_request(wsdl, &schema, text, request.operation.as_ref());
    for d in &result.diagnostics {
        eprintln!("{d}");
    }
    Ok(result)
}

/// Number of errors, for the summary line. Warnings do not block anything.
pub fn error_count(v: &Validation) -> usize {
    v.diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count()
}

pub fn command(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let request = support::find_request(&project, name)?;
    let text = project.read_request(request.id)?;
    let wsdl = support::load_project_wsdl(&project)?;
    let result = check(&wsdl, &request, &text)?;
    match error_count(&result) {
        0 => {
            println!("{name}: valid");
            Ok(ExitCode::SUCCESS)
        }
        n => {
            eprintln!("{name}: {n} error{}", if n == 1 { "" } else { "s" });
            Ok(ExitCode::from(1))
        }
    }
}
