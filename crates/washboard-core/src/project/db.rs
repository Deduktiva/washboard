//! `washboard.sqlite`: connection setup and schema migrations.
//!
//! The schema version is `PRAGMA user_version`. `MIGRATIONS[i]` takes a database from version
//! `i` to `i + 1`; each step runs in its own transaction together with the version bump, so an
//! interrupted upgrade leaves a consistent older version behind. Never edit a released step;
//! append a new one.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::ProjectError;

/// The version this build writes and understands.
pub const SCHEMA_VERSION: i32 = 1;

const MIGRATIONS: &[&str] = &[V1];

/// PLAN §3, schema v1.
const V1: &str = "
CREATE TABLE project  (id TEXT PRIMARY KEY,
                       name TEXT NOT NULL,
                       wsdl_path TEXT NOT NULL,
                       wsdl_imported_at TEXT NOT NULL,
                       history_limit INTEGER NOT NULL DEFAULT 20);
CREATE TABLE server   (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL,
                       ignore_tls_errors INTEGER NOT NULL DEFAULT 0,
                       auth_kind TEXT NOT NULL CHECK (auth_kind IN ('none','basic')),
                       username TEXT,
                       timeout_secs INTEGER NOT NULL DEFAULT 60,
                       sort_order INTEGER NOT NULL);
CREATE TABLE request  (id TEXT PRIMARY KEY,
                       file_name TEXT NOT NULL UNIQUE,
                       operation TEXT,
                       last_server_id TEXT REFERENCES server(id) ON DELETE SET NULL,
                       created_at TEXT NOT NULL, sort_order INTEGER NOT NULL);
CREATE TABLE history  (id TEXT PRIMARY KEY,
                       request_id TEXT NOT NULL REFERENCES request(id) ON DELETE CASCADE,
                       server_id TEXT, url TEXT NOT NULL, sent_at TEXT NOT NULL,
                       duration_ms INTEGER, http_status INTEGER, soap_fault INTEGER,
                       error TEXT,
                       response_headers TEXT,
                       file_stem TEXT NOT NULL);
CREATE INDEX history_by_request ON history(request_id, sent_at);
CREATE TABLE ui_state (key TEXT PRIMARY KEY, value TEXT);
";

/// Opens the database with the settings every connection needs.
///
/// Rollback journal, not WAL: the write volume is tiny and WAL side files behave badly in
/// synced or network folders. Foreign keys are off by default in SQLite and must be enabled
/// per connection.
pub(crate) fn connect(path: &Path, create: bool) -> Result<Connection, ProjectError> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    let _mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

pub(crate) fn user_version(conn: &Connection) -> Result<i32, ProjectError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Brings the schema to [`SCHEMA_VERSION`]. A newer database is refused, not touched.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), ProjectError> {
    let found = user_version(conn)?;
    if !(0..=SCHEMA_VERSION).contains(&found) {
        return Err(ProjectError::UnsupportedVersion {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (from, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", from as i32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
