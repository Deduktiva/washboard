//! `send`: validate, send through `washboard_core::http::send` (the only networking code),
//! record the exchange in the request's history (which also sets its last server), print the
//! status line to stderr and the response body, byte-exact, to stdout.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::bail;
use washboard_core::http::{self, SendRequest};
use washboard_core::model::{Auth, RequestMeta, Server};
use washboard_core::project::Project;
use washboard_core::validate::Validation;
use washboard_core::wsdl::Wsdl;

use crate::{passwords, support, validation};

pub fn run(
    dir: &Path,
    name: &str,
    server_name: Option<&str>,
    skip_validation: bool,
    fail_on_fault: bool,
) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let request = support::find_request(&project, name)?;
    let text = project.read_request(request.id)?;
    let wsdl = support::load_project_wsdl(&project)?;
    // With --skip-validation the request is still validated, so the errors are on record,
    // but nothing stops the send: sending broken requests on purpose is how servers get tested.
    let validation = match validation::check(&wsdl, &request, &text) {
        Ok(v) => {
            let errors = validation::error_count(&v);
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
            Some(v)
        }
        Err(e) if skip_validation => {
            eprintln!("note: not validated: {e:#}");
            None
        }
        Err(e) => return Err(e.context("not sent")),
    };
    let server = choose_server(&project, &request, server_name)?;
    let password = match &server.auth {
        Auth::Basic { username } => Some(passwords::for_send(&project, &server, username)?),
        Auth::None => None,
    };
    let soap_action = soap_action(&wsdl, &request, validation.as_ref());

    let exchange = http::send(&SendRequest {
        server: server.clone(),
        password,
        soap_action,
        body: text,
    });
    let fault = exchange
        .response
        .as_ref()
        .and_then(|r| http::detect_fault(&r.body));
    project.record_exchange(request.id, &server, &exchange, fault.is_some())?;

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

/// `--server`, else the request's last server, else the project's most recently used one,
/// else the only server.
fn choose_server(
    project: &Project,
    request: &RequestMeta,
    name: Option<&str>,
) -> anyhow::Result<Server> {
    if let Some(n) = name {
        return support::find_server(project, n);
    }
    if let Some(id) = request.last_server.or(project.last_used_server()?) {
        return Ok(project.server(id)?);
    }
    let mut all = project.servers()?;
    match all.len() {
        0 => bail!("the project has no servers; add one with `washboard server add`"),
        1 => Ok(all.remove(0)),
        _ => bail!("the request has no last server; choose one with --server"),
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
