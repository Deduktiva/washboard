//! `new-project` and `replace-wsdl`: the import check, then the project API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use washboard_core::diag;
use washboard_core::model::RequestId;
use washboard_core::project::{Project, ReplaceReport, format_timestamp, request_texts};
use washboard_core::validate::RequestSchema;

use crate::support::{self, short_time};

pub fn new(
    dir: &Path,
    entry: &Path,
    extra: &[PathBuf],
    name: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let checked = support::check_import(entry, extra)?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => std::path::absolute(dir)?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .context("cannot derive a project name from the folder; pass --name")?,
    };
    let project = Project::create(dir, &name, &checked.set)
        .with_context(|| format!("cannot create project in {}", dir.display()))?;
    println!(
        "created project {name:?} in {} ({} WSDL/XSD files)",
        project.root().display(),
        checked.set.files.len()
    );
    // PLAN §4: the address is only a suggestion; nothing connects until the user adds it.
    for (port, url) in checked.wsdl.soap11_addresses() {
        println!(
            "address of port {port}: {url}\n  add it with: washboard -C {} server add {port} {url}",
            shell_word(&dir.display().to_string())
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints what changed (PLAN §4 "Replace WSDL"): operations added and removed, and requests
/// that no longer validate against the new WSDL. Requests are never rewritten.
pub fn replace_wsdl(dir: &Path, entry: &Path, extra: &[PathBuf]) -> anyhow::Result<ExitCode> {
    let checked = support::check_import(entry, extra)?;
    let mut project = support::open_write(dir)?;
    let before = support::load_project_wsdl(&project)?.supported_operations();
    project.replace_wsdl(&checked.set)?;
    println!("replaced the WSDL; the previous set is in wsdl/.previous/");
    // `check_import` has made sure the set compiles; `None` only if the request wrapper
    // around it does not, and then the report checks well-formedness alone.
    let schema = RequestSchema::compile(&checked.wsdl.bundle).ok();
    let requests = request_texts(&project)?;
    let report = ReplaceReport::new(&before, &checked.wsdl, schema.as_ref(), &requests);
    for op in &report.added {
        println!("  added:   {op}");
    }
    for op in &report.removed {
        println!("  removed: {op}");
    }
    let names: HashMap<RequestId, String> = project
        .requests()?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();
    for id in &report.invalid {
        if let Some(name) = names.get(id) {
            println!("  no longer valid: {name:?}");
        }
    }
    if !report.invalid.is_empty() {
        println!(
            "see why with: washboard -C {} request validate <name>",
            shell_word(&dir.display().to_string())
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Read-only summary. The import check is re-run on the copied files, so it shows what the
/// app would see when compiling the project now.
pub fn show(dir: &Path) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let w = support::load_project_wsdl(&project)?;
    let errors = diag::error_count(&w.check.diagnostics);
    let warnings = w.check.diagnostics.len() - errors;
    let (ops, supported) = (w.report.operations, w.report.operations_supported);
    println!("name:        {}", project.name()?);
    println!(
        "folder:      {}",
        std::path::absolute(project.root())?.display()
    );
    println!("wsdl:        {}", project.wsdl_path()?);
    println!(
        "imported:    {}",
        short_time(&format_timestamp(project.wsdl_imported_at()?))
    );
    println!(
        "files:       {} ({} used)",
        w.report.files_supplied, w.report.files_used
    );
    println!("import check: {errors} errors, {warnings} warnings");
    for d in &w.check.diagnostics {
        println!("  {d}");
    }
    println!(
        "operations:  {ops} ({supported} supported, {} unsupported)",
        ops - supported
    );
    println!("requests:    {}", project.requests()?.len());
    println!("servers:     {}", project.servers()?.len());
    println!("history:     last {} per request", project.history_limit()?);
    Ok(ExitCode::SUCCESS)
}

/// Quotes for a POSIX shell when needed, so the printed command can be pasted.
fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
