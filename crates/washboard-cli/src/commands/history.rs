//! `history <request>`: entries newest first; `--show N` prints one entry's stored messages.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use serde::Serialize;
use washboard_core::model::ServerId;
use washboard_core::project::format_timestamp;

use crate::support::{self, short_time};

#[derive(Debug, Serialize)]
struct Row {
    id: String,
    sent_at: String,
    server: Option<String>,
    url: String,
    duration_ms: Option<u128>,
    http_status: Option<u16>,
    soap_fault: bool,
    error: Option<String>,
}

pub fn run(dir: &Path, name: &str, json: bool, show: Option<usize>) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let request = support::find_request(&project, name)?;
    let entries = project.history(request.id)?;

    if let Some(n) = show {
        let Some(entry) = n.checked_sub(1).and_then(|i| entries.get(i)) else {
            bail!(
                "{name:?} has {} history entries; no entry {n}",
                entries.len()
            );
        };
        let rec = project.load_history(entry.id)?;
        let mut out = io::stdout().lock();
        writeln!(out, "--- request sent to {}", rec.entry.url)?;
        out.write_all(&rec.request_body)?;
        writeln!(out)?;
        match &rec.response_body {
            Some(body) => {
                writeln!(out, "--- response")?;
                for (k, v) in &rec.response_headers {
                    writeln!(out, "{k}: {v}")?;
                }
                writeln!(out)?;
                out.write_all(body)?;
                writeln!(out)?;
            }
            None => writeln!(
                out,
                "--- no response: {}",
                rec.entry.error.as_deref().unwrap_or("not stored")
            )?,
        }
        return Ok(ExitCode::SUCCESS);
    }

    let servers: HashMap<ServerId, String> = project
        .servers()?
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    let rows: Vec<Row> = entries
        .iter()
        .map(|e| Row {
            id: e.id.to_string(),
            sent_at: format_timestamp(e.sent_at),
            server: e.server_id.and_then(|s| servers.get(&s).cloned()),
            url: e.url.clone(),
            duration_ms: e.duration.map(|d| d.as_millis()),
            http_status: e.http_status,
            soap_fault: e.soap_fault,
            error: e.error.clone(),
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).context("cannot write JSON")?
        );
        return Ok(ExitCode::SUCCESS);
    }
    if rows.is_empty() {
        println!("no history for {name:?}");
    }
    for (i, r) in rows.iter().enumerate() {
        let outcome = match (&r.error, r.http_status) {
            (Some(e), _) => format!("error: {e}"),
            (None, Some(s)) if r.soap_fault => format!("{s} SOAP fault"),
            (None, Some(s)) => s.to_string(),
            (None, None) => "-".to_owned(),
        };
        let ms = r.duration_ms.map(|d| format!("{d} ms")).unwrap_or_default();
        let target = r.server.as_deref().unwrap_or(&r.url);
        println!(
            "{:>3}  {}  {target}  {outcome}  {ms}",
            i + 1,
            short_time(&r.sent_at)
        );
    }
    Ok(ExitCode::SUCCESS)
}
