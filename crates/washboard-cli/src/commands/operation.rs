//! `operation list` (unsupported operations included and marked) and `operation template`.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use washboard_core::model::OperationRef;
use washboard_core::project::Project;
use washboard_core::wsdl::Protocol;

use crate::support::{self, protocol_label, style_label};

#[derive(Debug, Serialize)]
struct OperationRow {
    /// `{ns}Binding#Operation`, accepted wherever an operation is expected.
    id: String,
    binding: String,
    operation: String,
    protocol: &'static str,
    style: &'static str,
    soap_action: Option<String>,
    supported: bool,
    unsupported_reason: Option<String>,
}

pub fn list(dir: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let w = support::load_project_wsdl(&project)?;
    let mut rows = Vec::new();
    for b in &w.definitions.bindings {
        for o in &b.operations {
            let reason = o.support.reason().map(ToString::to_string);
            rows.push(OperationRow {
                id: format!("{}#{}", b.name, o.name),
                binding: b.name.local.clone(),
                operation: o.name.clone(),
                protocol: match b.protocol {
                    Protocol::Soap11 => "soap11",
                    Protocol::Soap12 => "soap12",
                    Protocol::Other { .. } => "other",
                },
                style: style_label(o.style),
                soap_action: o.soap_action.clone(),
                supported: reason.is_none(),
                unsupported_reason: reason,
            });
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(ExitCode::SUCCESS);
    }
    let mut rows = rows.iter();
    for b in &w.definitions.bindings {
        println!("{} ({})", b.name.local, protocol_label(&b.protocol));
        for row in rows.by_ref().take(b.operations.len()) {
            match &row.unsupported_reason {
                None => println!(
                    "  {:<24} {}{}",
                    row.operation,
                    row.style,
                    row.soap_action
                        .as_deref()
                        .filter(|a| !a.is_empty())
                        .map(|a| format!("  SOAPAction {a}"))
                        .unwrap_or_default()
                ),
                Some(reason) => println!("  {:<24} unsupported: {reason}", row.operation),
            }
        }
    }
    if w.definitions.bindings.is_empty() {
        println!("no operations");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn template(
    dir: &Path,
    spec: &str,
    save: bool,
    name: Option<&str>,
) -> anyhow::Result<ExitCode> {
    if save {
        let mut project = support::open_write(dir)?;
        let (op, text) = generate(&project, spec)?;
        let meta = match name {
            Some(n) => project.create_request_named(n, Some(&op), &text)?,
            None => project.create_request(&op, &text)?,
        };
        println!("{}", meta.name);
    } else {
        let project = support::open_read(dir)?;
        print!("{}", generate(&project, spec)?.1);
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolves the operation and builds its request envelope.
pub fn generate(project: &Project, spec: &str) -> anyhow::Result<(OperationRef, String)> {
    let w = support::load_project_wsdl(project)?;
    let op = support::resolve_operation(&w, spec)?;
    let text = support::envelope(&w, &op)?;
    Ok((op, text))
}
