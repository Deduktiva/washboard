//! `operation list` (unsupported operations included and marked) and `operation template`.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use washboard_core::model::{OperationRef, RequestMeta};
use washboard_core::project::{Project, ProjectSchema};
use washboard_core::schema::TemplateOptions;
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
    let w = support::load_project_schema(&project)?.wsdl;
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
        let generated = generate(&project, spec)?;
        let meta = generated.save(&mut project, name, &generated.text)?;
        println!("{}", meta.name);
    } else {
        let project = support::open_read(dir)?;
        print!("{}", generate(&project, spec)?.text);
    }
    Ok(ExitCode::SUCCESS)
}

/// An operation's request envelope, and the WSDL that says where New Request files it.
pub struct Generated {
    schema: ProjectSchema,
    operation: OperationRef,
    pub text: String,
}

impl Generated {
    /// Creates the request with `text` in the operation's folder, as the app's New Request
    /// does, named `name` or `<Operation> <n>`.
    pub fn save(
        &self,
        project: &mut Project,
        name: Option<&str>,
        text: &str,
    ) -> anyhow::Result<RequestMeta> {
        Ok(project.create_request_for(&self.schema.wsdl, &self.operation, None, name, text)?)
    }
}

/// Resolves the operation and builds its request envelope.
pub fn generate(project: &Project, spec: &str) -> anyhow::Result<Generated> {
    let schema = support::load_project_schema(project)?;
    let operation = support::resolve_operation(&schema.wsdl, spec)?;
    let text = schema.request_template(&operation, &TemplateOptions::default())?;
    Ok(Generated {
        schema,
        operation,
        text,
    })
}
