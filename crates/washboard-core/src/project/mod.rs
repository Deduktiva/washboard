//! Project folders: layout, `washboard.sqlite`, request files, history, reconciliation.
//!
//! Owned by WP-PROJECT (`docs/TASKS.md`). Layout and schema: `docs/PLAN.md` §3.
//!
//! A [`Project`] is an open project folder. Opening takes an exclusive lock on the folder, so
//! one `Project` value is the only writer; it is meant to live on the main thread (one per
//! window). All metadata changes go straight to SQLite; request XML lives in files that are
//! written atomically.
//!
//! ```text
//! <project>/
//! ├─ washboard.sqlite
//! ├─ .washboard.lock           flock target (see `fsutil::lock_folder`)
//! ├─ wsdl/                     entry WSDL + supporting files, structure preserved
//! │  └─ .previous/<timestamp>/ the set before the last "Replace WSDL"
//! ├─ requests/<name>.xml
//! └─ history/<request-uuid>/<stem>.request.xml, <stem>.response.xml
//! ```

mod app_state;
mod db;
mod fsutil;
mod history;
mod names;
mod requests;
mod servers;
mod timefmt;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{ProjectId, RequestId, ServerId};
use crate::secrets::SecretError;
use crate::wsdl::{Sources, Wsdl};
use crate::xml::DecodeError;

pub use app_state::{AppState, AppStateError, MAX_RECENT, OpenProject, STATE_FILE};
pub use db::SCHEMA_VERSION;
pub use history::{HistoryRecord, RequestHead};
pub use names::{NameError, validate_request_name};

use fsutil::{IoContext, join_rel, remove_dir_if_exists};

/// Database file name in the project folder.
pub const DB_FILE: &str = "washboard.sqlite";
/// Lock file name in the project folder.
pub const LOCK_FILE: &str = ".washboard.lock";
pub const WSDL_DIR: &str = "wsdl";
pub const REQUESTS_DIR: &str = "requests";
pub const HISTORY_DIR: &str = "history";
/// Inside `wsdl/`: the WSDL set before the last replacement.
pub const PREVIOUS_WSDL_DIR: &str = ".previous";
/// History entries kept per request unless the project says otherwise.
pub const DEFAULT_HISTORY_LIMIT: u32 = 20;

/// Keys in `ui_state` starting with this are used by the core itself.
pub const RESERVED_UI_STATE_PREFIX: &str = "washboard.";
/// The server most recently chosen or sent to; new requests start with it.
const LAST_SERVER_KEY: &str = "washboard.last_server";

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("project database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("the project in {} is already open in Washboard", .0.display())]
    AlreadyOpen(PathBuf),
    #[error("{} is not a Washboard project (no usable {DB_FILE})", .0.display())]
    NotAProject(PathBuf),
    #[error("{} already exists and is not an empty folder", .0.display())]
    FolderNotEmpty(PathBuf),
    #[error(
        "the project database has version {found}; this Washboard understands up to {supported}"
    )]
    UnsupportedVersion { found: i32, supported: i32 },
    #[error(
        "the project database has version {found}; open it for writing once to upgrade it to \
         {supported}"
    )]
    NeedsUpgrade { found: i32, supported: i32 },
    #[error("project name may not be empty")]
    EmptyProjectName,
    #[error("invalid request name {name:?}: {reason}")]
    InvalidName { name: String, reason: NameError },
    #[error("a request named {0:?} already exists")]
    NameTaken(String),
    #[error("invalid WSDL file set: {0}")]
    InvalidWsdlSet(String),
    #[error("no request with id {0}")]
    UnknownRequest(RequestId),
    #[error("no server with id {0}")]
    UnknownServer(ServerId),
    #[error("no history entry with id {0}")]
    UnknownHistory(crate::model::HistoryId),
    #[error("cannot read {}: {source}", path.display())]
    Decode { path: PathBuf, source: DecodeError },
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("the project database contains invalid data: {0}")]
    Corrupt(String),
}

pub type Result<T, E = ProjectError> = std::result::Result<T, E>;

/// One file to copy into `wsdl/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsdlFile {
    pub source: PathBuf,
    /// Destination relative to `wsdl/`, `/`-separated (e.g. `xsd/common/types.xsd`).
    /// Computed by the import check (WP-WSDL) so relative `schemaLocation`s keep working.
    pub dest: String,
}

/// The files of a WSDL import: the entry WSDL and everything it references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsdlSet {
    pub files: Vec<WsdlFile>,
    /// `dest` of the entry WSDL; must be one of `files`.
    pub entry: String,
}

impl WsdlSet {
    /// The files to copy for an import of `sources`: those the check found in use, at the
    /// destinations its layout chose.
    pub fn from_import(sources: &Sources, wsdl: &Wsdl) -> WsdlSet {
        let files = sources
            .files()
            .iter()
            .zip(&wsdl.layout)
            .zip(&wsdl.check.files)
            .filter(|(_, info)| info.used)
            .map(|((src, layout), _)| WsdlFile {
                source: PathBuf::from(&src.path),
                dest: layout.dest.clone(),
            })
            .collect();
        WsdlSet {
            files,
            entry: wsdl.entry_dest().to_owned(),
        }
    }
}

/// What [`Project::reconcile`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciliation {
    /// Request files that had no row (added in Finder, via git, …). Got a new row and id.
    pub added: Vec<RequestId>,
    /// Rows whose file is gone. Their rows (and history rows) are dropped; history files stay
    /// on disk until [`Project::prune_orphan_history`].
    pub removed: Vec<RequestId>,
}

/// An open project folder. Holds the folder lock until dropped.
#[derive(Debug)]
pub struct Project {
    root: PathBuf,
    conn: Connection,
    /// `None` only on file systems without `flock`.
    _lock: Option<File>,
    id: ProjectId,
    reconciliation: Reconciliation,
}

impl Project {
    /// Creates a project in `folder` (created if missing; an existing folder must be empty),
    /// copies the WSDL set byte-exact, and opens it.
    ///
    /// On failure, everything created is removed again.
    pub fn create(folder: &Path, name: &str, wsdl: &WsdlSet) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectError::EmptyProjectName);
        }
        validate_wsdl_set(wsdl)?;
        let existed = match fs::read_dir(folder) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(ProjectError::FolderNotEmpty(folder.to_owned()));
                }
                true
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(folder).at(folder)?;
                false
            }
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => {
                return Err(ProjectError::FolderNotEmpty(folder.to_owned()));
            }
            Err(e) => return Err(e).at(folder),
        };
        match Self::create_inner(folder, name, wsdl) {
            Ok(p) => Ok(p),
            Err(e) => {
                // Best effort: leave the folder as we found it.
                let _ = fs::remove_dir_all(folder);
                if existed {
                    let _ = fs::create_dir(folder);
                }
                Err(e)
            }
        }
    }

    fn create_inner(folder: &Path, name: &str, wsdl: &WsdlSet) -> Result<Project> {
        let lock = fsutil::lock_folder(folder, LOCK_FILE)?;
        for dir in [WSDL_DIR, REQUESTS_DIR, HISTORY_DIR] {
            let p = folder.join(dir);
            fs::create_dir(&p).at(&p)?;
        }
        copy_wsdl_set(wsdl, &folder.join(WSDL_DIR))?;
        let mut conn = db::connect(&folder.join(DB_FILE), true)?;
        db::migrate(&mut conn)?;
        let id = ProjectId::new();
        conn.execute(
            "INSERT INTO project (id, name, wsdl_path, wsdl_imported_at, history_limit)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                name,
                format!("{WSDL_DIR}/{}", wsdl.entry),
                timefmt::format(SystemTime::now()),
                DEFAULT_HISTORY_LIMIT
            ],
        )?;
        Ok(Project {
            root: folder.to_owned(),
            conn,
            _lock: lock,
            id,
            reconciliation: Reconciliation::default(),
        })
    }

    /// Opens an existing project, locks it, migrates the database, and reconciles request
    /// files with rows (see [`Project::reconciliation`]).
    pub fn open(folder: &Path) -> Result<Project> {
        let db_path = folder.join(DB_FILE);
        // Checked before locking so opening a random folder leaves no lock file behind.
        if !db_path.is_file() {
            return Err(ProjectError::NotAProject(folder.to_owned()));
        }
        let lock = fsutil::lock_folder(folder, LOCK_FILE)?;
        let mut conn = db::connect(&db_path, false)?;
        if db::user_version(&conn)? == 0 {
            return Err(ProjectError::NotAProject(folder.to_owned()));
        }
        db::migrate(&mut conn)?;
        let id: String = conn
            .query_row("SELECT id FROM project LIMIT 1", [], |r| r.get(0))
            .optional()?
            .ok_or_else(|| ProjectError::Corrupt("no project row".into()))?;
        let id = ProjectId(parse_uuid(&id, "project")?);
        // Empty folders are not tracked by git; recreate them.
        for dir in [WSDL_DIR, REQUESTS_DIR, HISTORY_DIR] {
            let p = folder.join(dir);
            fs::create_dir_all(&p).at(&p)?;
        }
        let mut project = Project {
            root: folder.to_owned(),
            conn,
            _lock: lock,
            id,
            reconciliation: Reconciliation::default(),
        };
        project.reconciliation = project.reconcile()?;
        Ok(project)
    }

    /// Opens an existing project for reading only: no folder lock, no migration, no
    /// reconciliation, nothing created. Works while another process (the app) has the project
    /// open, which is what the command-line tool's read-only commands need.
    ///
    /// Any write through the returned value fails with a database error. Request files added
    /// or removed since the last writable open are not reflected in [`Project::requests`]
    /// ([`Project::reconciliation`] is empty). A database older than [`SCHEMA_VERSION`] is
    /// refused with [`ProjectError::NeedsUpgrade`] rather than migrated.
    pub fn open_read_only(folder: &Path) -> Result<Project> {
        let db_path = folder.join(DB_FILE);
        if !db_path.is_file() {
            return Err(ProjectError::NotAProject(folder.to_owned()));
        }
        let conn = db::connect_read_only(&db_path)?;
        let found = db::user_version(&conn)?;
        if found == 0 {
            return Err(ProjectError::NotAProject(folder.to_owned()));
        }
        if found > SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedVersion {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < SCHEMA_VERSION {
            return Err(ProjectError::NeedsUpgrade {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        let id: String = conn
            .query_row("SELECT id FROM project LIMIT 1", [], |r| r.get(0))
            .optional()?
            .ok_or_else(|| ProjectError::Corrupt("no project row".into()))?;
        let id = ProjectId(parse_uuid(&id, "project")?);
        Ok(Project {
            root: folder.to_owned(),
            conn,
            _lock: None,
            id,
            reconciliation: Reconciliation::default(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stable across folder moves; the Keychain key prefix.
    pub fn id(&self) -> ProjectId {
        self.id
    }

    pub fn name(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT name FROM project", [], |r| r.get(0))?)
    }

    pub fn set_name(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectError::EmptyProjectName);
        }
        self.conn
            .execute("UPDATE project SET name = ?1", params![name])?;
        Ok(())
    }

    /// The entry WSDL, relative to the project root (`wsdl/…`, `/`-separated).
    pub fn wsdl_path(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT wsdl_path FROM project", [], |r| r.get(0))?)
    }

    /// Absolute path of the entry WSDL.
    pub fn entry_wsdl(&self) -> Result<PathBuf> {
        Ok(join_rel(&self.root, &self.wsdl_path()?))
    }

    /// The `wsdl/` folder; [`crate::model::SchemaOrigin`] paths are relative to it.
    pub fn wsdl_dir(&self) -> PathBuf {
        self.root.join(WSDL_DIR)
    }

    pub fn wsdl_imported_at(&self) -> Result<SystemTime> {
        let s: String = self
            .conn
            .query_row("SELECT wsdl_imported_at FROM project", [], |r| r.get(0))?;
        parse_time(&s)
    }

    /// The result of the reconciliation done by [`Project::open`].
    pub fn reconciliation(&self) -> &Reconciliation {
        &self.reconciliation
    }

    /// Replaces the WSDL set. The current contents of `wsdl/` move to
    /// `wsdl/.previous/<timestamp>/`, replacing any older previous set (one level is kept).
    ///
    /// The new files are staged first, so a missing source file leaves the project untouched.
    /// Requests are not modified.
    pub fn replace_wsdl(&mut self, wsdl: &WsdlSet) -> Result<()> {
        validate_wsdl_set(wsdl)?;
        let wsdl_dir = self.wsdl_dir();
        let staging = self
            .root
            .join(format!(".wb-staging-{}", Uuid::new_v4().simple()));
        fs::create_dir(&staging).at(&staging)?;
        let result = self.replace_wsdl_inner(wsdl, &wsdl_dir, &staging);
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn replace_wsdl_inner(
        &mut self,
        wsdl: &WsdlSet,
        wsdl_dir: &Path,
        staging: &Path,
    ) -> Result<()> {
        copy_wsdl_set(wsdl, staging)?;
        fs::create_dir_all(wsdl_dir).at(wsdl_dir)?;
        let previous = wsdl_dir.join(PREVIOUS_WSDL_DIR);
        remove_dir_if_exists(&previous)?;
        let now = SystemTime::now();
        let backup = previous.join(timefmt::compact(now));
        fs::create_dir_all(&backup).at(&backup)?;
        for entry in fs::read_dir(wsdl_dir).at(wsdl_dir)? {
            let entry = entry.at(wsdl_dir)?;
            if entry.file_name() == PREVIOUS_WSDL_DIR {
                continue;
            }
            let to = backup.join(entry.file_name());
            fs::rename(entry.path(), &to).at(&entry.path())?;
        }
        for entry in fs::read_dir(staging).at(staging)? {
            let entry = entry.at(staging)?;
            let to = wsdl_dir.join(entry.file_name());
            fs::rename(entry.path(), &to).at(&to)?;
        }
        self.conn.execute(
            "UPDATE project SET wsdl_path = ?1, wsdl_imported_at = ?2",
            params![format!("{WSDL_DIR}/{}", wsdl.entry), timefmt::format(now)],
        )?;
        Ok(())
    }

    /// Makes the `request` table match the `*.xml` files in `requests/`.
    ///
    /// Files without a row get one (no operation, appended to the order). Rows without a file
    /// are dropped. A row whose file differs only in letter case (renamed in Finder on a
    /// case-insensitive volume) is updated instead, keeping its id and history.
    pub fn reconcile(&mut self) -> Result<Reconciliation> {
        let dir = self.root.join(REQUESTS_DIR);
        let mut on_disk: Vec<String> = Vec::new();
        for entry in fs::read_dir(&dir).at(&dir)? {
            let entry = entry.at(&dir)?;
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            if names::stem_of(&file_name).is_none() || !entry.path().is_file() {
                continue;
            }
            on_disk.push(file_name);
        }
        on_disk.sort();

        let rows: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, file_name FROM request")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let disk_exact: HashSet<&str> = on_disk.iter().map(String::as_str).collect();
        let mut disk_folded: HashMap<String, &str> = HashMap::new();
        for f in &on_disk {
            disk_folded.entry(names::fold(f)).or_insert(f);
        }
        let mut claimed: HashSet<&str> = HashSet::new();
        let mut renames: Vec<(String, &str)> = Vec::new();
        let mut removed_ids: Vec<String> = Vec::new();
        for (_, file) in &rows {
            if disk_exact.contains(file.as_str()) {
                claimed.insert(file);
            }
        }
        for (id, file) in &rows {
            if disk_exact.contains(file.as_str()) {
                continue;
            }
            match disk_folded.get(&names::fold(file)) {
                Some(f) if !claimed.contains(f) => {
                    claimed.insert(f);
                    renames.push((id.clone(), f));
                }
                _ => removed_ids.push(id.clone()),
            }
        }

        let tx = self.conn.transaction()?;
        let mut result = Reconciliation::default();
        for id in &removed_ids {
            tx.execute("DELETE FROM request WHERE id = ?1", params![id])?;
            if let Ok(u) = Uuid::parse_str(id) {
                result.removed.push(RequestId(u));
            }
        }
        for (id, file) in &renames {
            tx.execute(
                "UPDATE request SET file_name = ?1 WHERE id = ?2",
                params![file, id],
            )?;
        }
        let mut order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) FROM request",
            [],
            |r| r.get(0),
        )?;
        let now = timefmt::format(SystemTime::now());
        for file in on_disk.iter().filter(|f| !claimed.contains(f.as_str())) {
            order += 1;
            let id = RequestId::new();
            tx.execute(
                "INSERT INTO request (id, file_name, operation, last_server_id, created_at,
                                      sort_order)
                 VALUES (?1, ?2, NULL, NULL, ?3, ?4)",
                params![id.to_string(), file, now, order],
            )?;
            result.added.push(id);
        }
        tx.commit()?;
        Ok(result)
    }

    /// A value the UI stored for this project (split positions, selection, …).
    pub fn ui_state(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM ui_state WHERE key = ?1",
                params![key],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Stores (`Some`) or removes (`None`) a UI value. Keys starting with
    /// [`RESERVED_UI_STATE_PREFIX`] belong to the core.
    pub fn set_ui_state(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO ui_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => self
                .conn
                .execute("DELETE FROM ui_state WHERE key = ?1", params![key])?,
        };
        Ok(())
    }
}

/// Checks destinations before anything is copied.
///
/// Destinations must be relative, `/`-separated, without `.`/`..`/empty components, and no
/// component may start with `.` (hidden names in `wsdl/` are Washboard's, e.g. `.previous`).
/// Two destinations that differ only in case are rejected (same file on macOS).
fn validate_wsdl_set(set: &WsdlSet) -> Result<()> {
    let bad = |msg: String| Err(ProjectError::InvalidWsdlSet(msg));
    if set.files.is_empty() {
        return bad("no files".into());
    }
    let mut seen = HashSet::new();
    for f in &set.files {
        let d = &f.dest;
        if d.is_empty() || d.starts_with('/') || d.contains('\\') || d.contains('\0') {
            return bad(format!("bad destination {d:?}"));
        }
        if d.split('/').any(|c| c.is_empty() || c.starts_with('.')) {
            return bad(format!("bad destination {d:?}"));
        }
        if !seen.insert(names::fold(d)) {
            return bad(format!("duplicate destination {d:?}"));
        }
    }
    if !set.files.iter().any(|f| f.dest == set.entry) {
        return bad(format!("entry {:?} is not among the files", set.entry));
    }
    Ok(())
}

/// Copies byte-exact; WSDLs with BOMs or UTF-16 stay as they are.
fn copy_wsdl_set(set: &WsdlSet, into: &Path) -> Result<()> {
    for f in &set.files {
        let to = join_rel(into, &f.dest);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).at(parent)?;
        }
        fs::copy(&f.source, &to).at(&f.source)?;
    }
    Ok(())
}

/// A timestamp as RFC 3339 UTC text, in the fixed-width form the database stores
/// (`YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`). For the command-line tool, which has no date library.
pub fn format_timestamp(t: SystemTime) -> String {
    timefmt::format(t)
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|_| ProjectError::Corrupt(format!("invalid {what} id {s:?}")))
}

fn parse_time(s: &str) -> Result<SystemTime> {
    timefmt::parse(s).ok_or_else(|| ProjectError::Corrupt(format!("invalid timestamp {s:?}")))
}
