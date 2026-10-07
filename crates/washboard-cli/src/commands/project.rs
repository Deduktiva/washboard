//! `new-project` and `replace-wsdl`: the import check, then the project API.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use washboard_core::diag::Severity;
use washboard_core::project::{Project, format_timestamp};
use washboard_core::wsdl::{Protocol, Wsdl};

use crate::support::{self, op_ref_text, short_time};

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
    for (port, url) in soap11_addresses(&checked.wsdl) {
        println!(
            "address of port {port}: {url}\n  add it with: washboard -C {} server add {port} {url}",
            shell_word(&dir.display().to_string())
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub fn replace_wsdl(dir: &Path, entry: &Path, extra: &[PathBuf]) -> anyhow::Result<ExitCode> {
    let checked = support::check_import(entry, extra)?;
    let mut project = support::open_write(dir)?;
    let before = supported_ops(&support::load_project_wsdl(&project)?);
    project.replace_wsdl(&checked.set)?;
    let after = supported_ops(&checked.wsdl);
    println!("replaced the WSDL; the previous set is in wsdl/.previous/");
    for op in after.difference(&before) {
        println!("  added:   {op}");
    }
    for op in before.difference(&after) {
        println!("  removed: {op}");
    }
    let stale: Vec<String> = project
        .requests()?
        .into_iter()
        .filter(|r| {
            r.operation
                .as_ref()
                .is_some_and(|o| !after.contains(&op_ref_text(o)))
        })
        .map(|r| r.name)
        .collect();
    for name in stale {
        println!("  request {name:?} was created for an operation that no longer exists");
    }
    Ok(ExitCode::SUCCESS)
}

/// Read-only summary. The import check is re-run on the copied files, so it shows what the
/// app would see when compiling the project now.
pub fn show(dir: &Path) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let w = support::load_project_wsdl(&project)?;
    let (errors, warnings) =
        w.check
            .diagnostics
            .iter()
            .fold((0, 0), |(e, wn), d| match d.severity {
                Severity::Error => (e + 1, wn),
                Severity::Warning => (e, wn + 1),
            });
    let ops: Vec<_> = w
        .definitions
        .bindings
        .iter()
        .flat_map(|b| &b.operations)
        .collect();
    let supported = ops.iter().filter(|o| o.is_supported()).count();
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
        "operations:  {} ({supported} supported, {} unsupported)",
        ops.len(),
        ops.len() - supported
    );
    println!("requests:    {}", project.requests()?.len());
    println!("servers:     {}", project.servers()?.len());
    println!("history:     last {} per request", project.history_limit()?);
    Ok(ExitCode::SUCCESS)
}

fn supported_ops(w: &Wsdl) -> BTreeSet<String> {
    w.definitions
        .bindings
        .iter()
        .flat_map(|b| {
            b.operations
                .iter()
                .filter(|o| o.is_supported())
                .map(|o| format!("{}#{}", b.name, o.name))
        })
        .collect()
}

/// `(port name, address)` of every port bound to a SOAP 1.1 binding.
fn soap11_addresses(w: &Wsdl) -> Vec<(String, String)> {
    w.definitions
        .services
        .iter()
        .flat_map(|s| &s.ports)
        .filter(|p| {
            w.definitions
                .binding(&p.binding)
                .is_some_and(|b| b.protocol == Protocol::Soap11)
        })
        .filter_map(|p| Some((p.name.clone(), p.address.clone()?)))
        .collect()
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
