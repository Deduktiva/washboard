//! Per-request response history: `history/<request-uuid>/<stem>.{request,response}.xml` plus a
//! `history` row each. Keyed by request id, so renames do not orphan it.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::fsutil::{IoContext, atomic_write, remove_dir_if_exists, remove_file_if_exists};
use super::{
    DEFAULT_HISTORY_LIMIT, HISTORY_DIR, LAST_SERVER_KEY, Project, ProjectError, Result, parse_time,
    parse_uuid, timefmt,
};
use crate::http::{Exchange, RawMessage};
use crate::model::{HistoryEntry, HistoryId, RequestId, Server, ServerId};

const REQUEST_SUFFIX: &str = ".request.xml";
const RESPONSE_SUFFIX: &str = ".response.xml";

/// A history entry with its stored messages, for the History tab and "Restore request".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecord {
    pub entry: HistoryEntry,
    /// The request body exactly as sent.
    pub request_body: Vec<u8>,
    /// `None` when no response was received (transport error) or its file is gone.
    pub response_body: Option<Vec<u8>>,
    /// In received order. Empty when no response was received.
    pub response_headers: Vec<(String, String)>,
}

const COLUMNS: &str = "id, request_id, server_id, url, sent_at, duration_ms, http_status, \
                       soap_fault, error, response_headers, file_stem";

struct RawRow {
    id: String,
    request_id: String,
    server_id: Option<String>,
    url: String,
    sent_at: String,
    duration_ms: Option<i64>,
    http_status: Option<i64>,
    soap_fault: Option<bool>,
    error: Option<String>,
    response_headers: Option<String>,
    file_stem: String,
}

fn raw_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: r.get(0)?,
        request_id: r.get(1)?,
        server_id: r.get(2)?,
        url: r.get(3)?,
        sent_at: r.get(4)?,
        duration_ms: r.get(5)?,
        http_status: r.get(6)?,
        soap_fault: r.get(7)?,
        error: r.get(8)?,
        response_headers: r.get(9)?,
        file_stem: r.get(10)?,
    })
}

fn to_entry(r: &RawRow) -> Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: HistoryId(parse_uuid(&r.id, "history")?),
        request_id: RequestId(parse_uuid(&r.request_id, "request")?),
        // History keeps the id of deleted servers as a plain value; a bad one is just dropped.
        server_id: r
            .server_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(ServerId),
        url: r.url.clone(),
        sent_at: parse_time(&r.sent_at)?,
        duration: r
            .duration_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(Duration::from_millis),
        http_status: r.http_status.and_then(|s| u16::try_from(s).ok()),
        soap_fault: r.soap_fault.unwrap_or(false),
        error: r.error.clone(),
    })
}

/// The stem must stay inside the request's history folder.
fn valid_stem(stem: &str) -> bool {
    !stem.is_empty() && !stem.starts_with('.') && !stem.contains(['/', '\\', '\0'])
}

impl Project {
    fn history_dir(&self, request: RequestId) -> PathBuf {
        self.root.join(HISTORY_DIR).join(request.to_string())
    }

    /// Entries kept per request.
    pub fn history_limit(&self) -> Result<u32> {
        let n: i64 = self
            .conn
            .query_row("SELECT history_limit FROM project", [], |r| r.get(0))?;
        Ok(u32::try_from(n).unwrap_or(DEFAULT_HISTORY_LIMIT))
    }

    /// Changes the retention (at least 1) and prunes every request to it.
    pub fn set_history_limit(&mut self, limit: u32) -> Result<()> {
        let limit = limit.max(1);
        self.conn
            .execute("UPDATE project SET history_limit = ?1", params![limit])?;
        for r in self.requests()? {
            self.prune_history(r.id)?;
        }
        Ok(())
    }

    /// Stores a sent exchange for `request`: the request body and response body as files, the
    /// metadata as a row. Also makes `server` the request's and the project's last server, and
    /// prunes the request's history to the project limit.
    ///
    /// `soap_fault` comes from the caller (the HTTP module's fault detection).
    pub fn record_exchange(
        &mut self,
        request: RequestId,
        server: &Server,
        exchange: &Exchange,
        soap_fault: bool,
    ) -> Result<HistoryEntry> {
        self.request(request)?;
        let dir = self.history_dir(request);
        fs::create_dir_all(&dir).at(&dir)?;
        let base = timefmt::compact(exchange.started_at);
        let stem = (1u32..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|s| {
                !dir.join(format!("{s}{REQUEST_SUFFIX}")).exists()
                    && !dir.join(format!("{s}{RESPONSE_SUFFIX}")).exists()
            })
            .unwrap_or(base);
        let req_path = dir.join(format!("{stem}{REQUEST_SUFFIX}"));
        let resp_path = dir.join(format!("{stem}{RESPONSE_SUFFIX}"));
        atomic_write(&req_path, &exchange.request.body)?;
        if let Some(resp) = &exchange.response
            && let Err(e) = atomic_write(&resp_path, &resp.body)
        {
            let _ = fs::remove_file(&req_path);
            return Err(e);
        }

        let entry = HistoryEntry {
            id: HistoryId::new(),
            request_id: request,
            server_id: Some(server.id),
            url: server.url.clone(),
            sent_at: exchange.started_at,
            duration: Some(exchange.duration),
            http_status: exchange.response.as_ref().and_then(RawMessage::status_code),
            soap_fault,
            error: exchange.error.clone(),
        };
        let headers = exchange
            .response
            .as_ref()
            .map(|r| serde_json::to_string(&r.headers))
            .transpose()
            .map_err(|e| ProjectError::Corrupt(format!("cannot encode headers: {e}")))?;
        let inserted = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            tx.execute(
                &format!("INSERT INTO history ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
                params![
                    entry.id.to_string(),
                    request.to_string(),
                    server.id.to_string(),
                    entry.url,
                    timefmt::format(entry.sent_at),
                    i64::try_from(exchange.duration.as_millis()).unwrap_or(i64::MAX),
                    entry.http_status,
                    soap_fault,
                    entry.error,
                    headers,
                    stem
                ],
            )?;
            // Only servers that are (still) part of the project become "last server".
            let known = tx.execute(
                "UPDATE request SET last_server_id = ?1
                 WHERE id = ?2 AND EXISTS (SELECT 1 FROM server WHERE id = ?1)",
                params![server.id.to_string(), request.to_string()],
            )?;
            if known > 0 {
                tx.execute(
                    "INSERT INTO ui_state (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![LAST_SERVER_KEY, server.id.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(e) = inserted {
            let _ = fs::remove_file(&req_path);
            let _ = fs::remove_file(&resp_path);
            return Err(e);
        }
        self.prune_history(request)?;
        Ok(entry)
    }

    /// The request's history, newest first.
    pub fn history(&self, request: RequestId) -> Result<Vec<HistoryEntry>> {
        let rows: Vec<RawRow> = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM history WHERE request_id = ?1
                 ORDER BY sent_at DESC, rowid DESC"
            ))?;
            stmt.query_map(params![request.to_string()], raw_row)?
                .collect::<rusqlite::Result<_>>()?
        };
        rows.iter().map(to_entry).collect()
    }

    /// One entry with its stored messages.
    pub fn load_history(&self, id: HistoryId) -> Result<HistoryRecord> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM history WHERE id = ?1"),
                params![id.to_string()],
                raw_row,
            )
            .optional()?
            .ok_or(ProjectError::UnknownHistory(id))?;
        let entry = to_entry(&row)?;
        if !valid_stem(&row.file_stem) {
            return Err(ProjectError::Corrupt(format!(
                "bad history file stem {:?}",
                row.file_stem
            )));
        }
        let dir = self.history_dir(entry.request_id);
        let req_path = dir.join(format!("{}{REQUEST_SUFFIX}", row.file_stem));
        let request_body = fs::read(&req_path).at(&req_path)?;
        let resp_path = dir.join(format!("{}{RESPONSE_SUFFIX}", row.file_stem));
        let response_body = match fs::read(&resp_path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).at(&resp_path),
        };
        // Headers are informational; unreadable JSON shows as no headers rather than failing.
        let response_headers = row
            .response_headers
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        Ok(HistoryRecord {
            entry,
            request_body,
            response_body,
            response_headers,
        })
    }

    /// Drops the oldest entries (rows and files) beyond the project's limit.
    pub fn prune_history(&mut self, request: RequestId) -> Result<()> {
        let limit = self.history_limit()?;
        let doomed: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, file_stem FROM history WHERE request_id = ?1
                 ORDER BY sent_at DESC, rowid DESC LIMIT -1 OFFSET ?2",
            )?;
            stmt.query_map(params![request.to_string(), limit], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        if doomed.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for (id, _) in &doomed {
            tx.execute("DELETE FROM history WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        let dir = self.history_dir(request);
        for (_, stem) in doomed.iter().filter(|(_, s)| valid_stem(s)) {
            remove_file_if_exists(&dir.join(format!("{stem}{REQUEST_SUFFIX}")))?;
            remove_file_if_exists(&dir.join(format!("{stem}{RESPONSE_SUFFIX}")))?;
        }
        Ok(())
    }

    /// Removes history files no row refers to: folders of requests that no longer exist (e.g.
    /// dropped by reconciliation) and stray files left by an interrupted write. Returns the
    /// number of folders and files removed. Names Washboard did not create are left alone.
    pub fn prune_orphan_history(&mut self) -> Result<usize> {
        let root = self.root.join(HISTORY_DIR);
        let rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT request_id, file_stem FROM history")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let requests: HashSet<String> = {
            let mut stmt = self.conn.prepare("SELECT id FROM request")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let stems: HashSet<(String, String)> = rows.into_iter().collect();
        let mut removed = 0;
        let entries = match fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).at(&root),
        };
        for entry in entries {
            let entry = entry.at(&root)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(uuid) = uuid::Uuid::parse_str(&name) else {
                continue;
            };
            let req = uuid.to_string();
            if !entry.path().is_dir() {
                continue;
            }
            if !requests.contains(&req) {
                remove_dir_if_exists(&entry.path())?;
                removed += 1;
                continue;
            }
            let dir = entry.path();
            for file in fs::read_dir(&dir).at(&dir)? {
                let file = file.at(&dir)?;
                let Some(fname) = file.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let stem = fname
                    .strip_suffix(REQUEST_SUFFIX)
                    .or_else(|| fname.strip_suffix(RESPONSE_SUFFIX));
                if let Some(stem) = stem
                    && !stems.contains(&(req.clone(), stem.to_owned()))
                {
                    remove_file_if_exists(&file.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}
