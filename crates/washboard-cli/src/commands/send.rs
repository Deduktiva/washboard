//! `send`: fill in the placeholders, validate, send through `washboard_core::http::send` (the
//! only networking code), record the exchange in the request's history, print the status line
//! to stderr and the response body, byte-exact, to stdout. `--server` and `--set` apply to this
//! send only; they change neither the request's pins nor the project's choice.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::bail;
use washboard_core::diag;
use washboard_core::http::{self, SendRequest};
use washboard_core::interpolate;
use washboard_core::model::{Auth, RequestMeta, Server};
use washboard_core::project::Project;
use washboard_core::soap;
use washboard_core::validate::Validation;
use washboard_core::wsdl::Wsdl;

use crate::{passwords, support, validation};

pub fn run(
    dir: &Path,
    name: &str,
    server_name: Option<&str>,
    set_name: Option<&str>,
    skip_validation: bool,
    fail_on_fault: bool,
) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let request = support::find_request(&project, name)?;
    let text = project.read_request(request.id)?;
    let schema = support::load_project_schema(&project)?;
    let (parameter_set, values) = support::parameter_values(&project, &request, set_name)?;
    // With --skip-validation the request is still validated, so the errors are on record,
    // but schema errors do not stop the send: sending broken requests on purpose is how
    // servers get tested. Placeholders that cannot be filled in always do: `${…}` text is never
    // what anyone means to send.
    let (body, validation) = match validation::check(&schema, &request, &text, &values) {
        Ok(v) => {
            if v.resolved.has_errors() {
                return Ok(not_sent_placeholders(&v.resolved));
            }
            let errors = diag::error_count(&v.validation.diagnostics);
            let s = if errors == 1 { "" } else { "s" };
            if errors > 0 && !skip_validation {
                eprintln!(
                    "not sent: the request has {errors} error{s} (--skip-validation sends anyway)"
                );
                return Ok(ExitCode::from(1));
            }
            if errors > 0 {
                eprintln!("note: sending despite {errors} error{s} (--skip-validation)");
            }
            (v.resolved.text, Some(v.validation))
        }
        Err(e) if skip_validation => {
            eprintln!("note: not validated: {e:#}");
            let resolved = interpolate::resolve(&text, &values);
            if resolved.has_errors() {
                validation::print_diagnostics(&request.name, &text, &resolved.diagnostics);
                return Ok(not_sent_placeholders(&resolved));
            }
            (resolved.text, None)
        }
        Err(e) => return Err(e.context("not sent")),
    };
    let server = choose_server(&project, &request, server_name)?;
    let password = match &server.auth {
        Auth::Basic { username } => Some(passwords::for_send(&project, &server, username)?),
        Auth::None => None,
    };
    let soap_action = soap_action(&schema.wsdl, &request, validation.as_ref());

    let exchange = http::send(&SendRequest {
        server: server.clone(),
        password,
        soap_action,
        body,
    });
    let fault = exchange
        .response
        .as_ref()
        .and_then(|r| soap::detect_fault(&r.body));
    project.record_exchange(
        request.id,
        &server,
        parameter_set,
        &exchange,
        fault.is_some(),
    )?;

    let Some(response) = &exchange.response else {
        let err = exchange.error.as_deref().unwrap_or("no response");
        eprintln!("{} → {err}", server.url);
        return Ok(ExitCode::from(1));
    };
    eprintln!(
        "{}  {} ms  {} bytes",
        response.start_line,
        exchange.duration.as_millis(),
        response.body.len()
    );
    if let Some(e) = &exchange.error {
        // E.g. a truncated oversized body: a response arrived but is incomplete.
        eprintln!("warning: {e}");
    }
    let mut out = io::stdout().lock();
    out.write_all(&response.body)?;
    if !response.body.ends_with(b"\n") {
        out.write_all(b"\n")?;
    }
    out.flush()?;
    if let Some(f) = fault {
        eprintln!("SOAP fault: {}: {}", f.code, f.string);
        if fail_on_fault {
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Refuses a send whose placeholders could not all be filled in (already printed), with or
/// without `--skip-validation`, which skips schema validation only.
fn not_sent_placeholders(resolved: &interpolate::Resolved) -> ExitCode {
    let errors = diag::error_count(&resolved.diagnostics);
    let s = if errors == 1 { "" } else { "s" };
    eprintln!("not sent: {errors} placeholder{s} could not be filled in");
    ExitCode::from(1)
}

/// `--server`, else the request's pin, else the project's server, else the first
/// ([`Project::selection`]).
fn choose_server(
    project: &Project,
    request: &RequestMeta,
    name: Option<&str>,
) -> anyhow::Result<Server> {
    if let Some(n) = name {
        return support::find_server(project, n);
    }
    match project.selection(Some(request))?.server {
        Some(id) => Ok(project.server(id)?),
        None => bail!("the project has no servers; add one with `washboard server add`"),
    }
}

/// The operation the body dispatched to during validation; falls back to the request's
/// operation hint when nothing dispatched (only possible with `--skip-validation`, e.g. for a
/// body that does not parse). `None` sends `SOAPAction: ""`.
fn soap_action(wsdl: &Wsdl, request: &RequestMeta, v: Option<&Validation>) -> Option<String> {
    match v.and_then(|v| v.dispatched.first()) {
        Some(d) => d.soap_action.clone(),
        None => request
            .operation
            .as_ref()
            .and_then(|h| wsdl.operation(h))
            .and_then(|(_, op)| op.soap_action.clone()),
    }
}
