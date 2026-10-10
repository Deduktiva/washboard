//! `request list|new|show|rename|duplicate|delete|format` (`validate`, `send` and `history` live in
//! their own modules).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;
use serde::Serialize;
use washboard_core::model::{OperationRef, ServerId};
use washboard_core::xml;

use crate::commands::operation;
use crate::support;

#[derive(Debug, Serialize)]
struct RequestRow {
    id: String,
    name: String,
    /// `{ns}Binding#Operation` the request was created for; a hint that may be stale.
    operation: Option<String>,
    last_server: Option<String>,
}

/// Requests as recorded in the database. Read-only, so files added in Finder since the last
/// writable open are not listed yet.
pub fn list(dir: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let servers: HashMap<ServerId, String> = project
        .servers()?
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    let rows: Vec<RequestRow> = project
        .requests()?
        .into_iter()
        .map(|r| RequestRow {
            id: r.id.to_string(),
            name: r.name,
            operation: r.operation.as_ref().map(OperationRef::to_string),
            last_server: r.last_server.and_then(|s| servers.get(&s).cloned()),
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(ExitCode::SUCCESS);
    }
    if rows.is_empty() {
        println!("no requests");
    }
    for r in &rows {
        let op = r
            .operation
            .as_deref()
            .map(|o| o.rsplit_once('}').map_or(o, |(_, rest)| rest))
            .unwrap_or("-");
        let server = r
            .last_server
            .as_deref()
            .map(|s| format!("  server {s}"))
            .unwrap_or_default();
        println!("{:<24} {op}{server}", r.name);
    }
    Ok(ExitCode::SUCCESS)
}

/// The content comes from the operation's template, or from `--from` (decoded like any XML
/// input, so a UTF-16 file works). The name defaults to `<Operation> <n>`.
pub fn new(
    dir: &Path,
    spec: &str,
    name: Option<&str>,
    from: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let (op, template) = operation::generate(&project, spec)?;
    let text = match from {
        Some(path) => {
            let bytes = if path == Path::new("-") {
                let mut b = Vec::new();
                io::stdin()
                    .read_to_end(&mut b)
                    .context("cannot read stdin")?;
                b
            } else {
                std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?
            };
            xml::decode(&bytes)
                .with_context(|| format!("cannot decode {}", path.display()))?
                .text
        }
        None => template,
    };
    let meta = match name {
        Some(n) => project.create_request_named(n, Some(&op), &text)?,
        None => project.create_request(&op, &text)?,
    };
    println!("{}", meta.name);
    Ok(ExitCode::SUCCESS)
}

pub fn rename(dir: &Path, name: &str, new_name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let r = support::find_request(&project, name)?;
    project.rename_request(r.id, new_name)?;
    Ok(ExitCode::SUCCESS)
}

pub fn duplicate(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let r = support::find_request(&project, name)?;
    let copy = project.duplicate_request(r.id)?;
    println!("{}", copy.name);
    Ok(ExitCode::SUCCESS)
}

pub fn delete(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let r = support::find_request(&project, name)?;
    project.delete_request(r.id)?;
    Ok(ExitCode::SUCCESS)
}

pub fn show(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let r = support::find_request(&project, name)?;
    let text = project.read_request(r.id)?;
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Format XML at `indent` spaces, as the app's ⌃I does. Rewrites the file only if that changes
/// it; with `check`, writes nothing and exits 1 if it would. A request that is not well-formed
/// is left alone and exits 1. The app's indent setting is not read: it lives in the macOS user
/// defaults, which the CLI does not use.
pub fn format(dir: &Path, name: &str, indent: usize, check: bool) -> anyhow::Result<ExitCode> {
    let project = if check {
        support::open_read(dir)?
    } else {
        support::open_write(dir)?
    };
    let r = support::find_request(&project, name)?;
    let text = project.read_request(r.id)?;
    let formatted = match xml::pretty_print(&text, indent) {
        Ok(formatted) => formatted,
        Err(d) => {
            eprintln!("{name}:{d}");
            return Ok(ExitCode::from(1));
        }
    };
    if formatted == text {
        return Ok(ExitCode::SUCCESS);
    }
    if check {
        eprintln!("{name}: not formatted");
        return Ok(ExitCode::from(1));
    }
    project.write_request(r.id, &formatted)?;
    Ok(ExitCode::SUCCESS)
}
