//! Request validation. Waits for WP-VALIDATE's pipeline (`docs/TASKS.md`).
//!
//! Both `validate` and `send` go through [`check`], so wiring in the real pipeline is a change
//! to that one function (and dropping `send --skip-validation`).

use std::path::Path;
use std::process::ExitCode;

use anyhow::bail;
use washboard_core::model::RequestMeta;
use washboard_core::project::Project;
use washboard_core::wsdl::Wsdl;

use crate::support;

/// Validates `text` (the request's content) against the project's WSDL. `Ok` means no errors;
/// diagnostics are printed to stderr.
pub fn check(
    _project: &Project,
    _wsdl: &Wsdl,
    _request: &RequestMeta,
    _text: &str,
) -> anyhow::Result<()> {
    bail!("request validation is not available yet")
}

pub fn command(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let request = support::find_request(&project, name)?;
    let text = project.read_request(request.id)?;
    let wsdl = support::load_project_wsdl(&project)?;
    check(&project, &wsdl, &request, &text)?;
    println!("{name}: valid");
    Ok(ExitCode::SUCCESS)
}
