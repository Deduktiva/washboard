//! Requests: rows in `request`, XML in `requests/<folder>/<name>.xml`; and the folders, which
//! are subdirectories of `requests/` and exist only on disk.
//!
//! `request.file_name` is the file's path relative to `requests/`, always `/`-separated.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use super::fsutil::{
    IoContext, atomic_write, join_rel, remove_dir_if_exists, remove_file_if_exists,
};
use super::names;
use super::{
    HISTORY_DIR, Project, ProjectError, REQUESTS_DIR, Result, parse_time, parse_uuid, timefmt,
};
use crate::model::{OperationRef, ParameterSetId, RequestId, RequestMeta, ServerId};
use crate::wsdl::Wsdl;
use crate::xml;

/// The server metadata a new request row starts with.
#[derive(Default)]
struct NewRequest {
    last_server: Option<ServerId>,
    pinned_server: Option<ServerId>,
    pinned_parameter_set: Option<ParameterSetId>,
}

/// The name New Folder gives, numbered from 2 when taken, as in Finder.
const UNTITLED_FOLDER: &str = "New Folder";

/// What `requests/` holds, as `/`-separated paths relative to it. Names starting with `.` are
/// skipped (temp files, `.git`, `.DS_Store`), and so are symlinked folders, which could loop.
#[derive(Debug, Default)]
pub(super) struct Listing {
    pub(super) files: Vec<String>,
    pub(super) folders: Vec<String>,
}

/// Walks `requests/` recursively; both lists come out sorted.
pub(super) fn walk(dir: &Path) -> Result<Listing> {
    let mut listing = Listing::default();
    walk_into(dir, "", &mut listing)?;
    listing.files.sort();
    listing.folders.sort();
    Ok(listing)
}

fn walk_into(dir: &Path, rel: &str, listing: &mut Listing) -> Result<()> {
    for entry in fs::read_dir(dir).at(dir)? {
        let entry = entry.at(dir)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = names::join(rel, &name);
        if entry.file_type().at(&entry.path())?.is_dir() {
            walk_into(&entry.path(), &path, listing)?;
            listing.folders.push(path);
        } else if names::stem_of(&name).is_some() && entry.path().is_file() {
            listing.files.push(path);
        }
    }
    Ok(())
}

/// The entry of `dir` whose name equals `name` ignoring case, as spelled on disk; an exact
/// match wins. `None` if there is none or `dir` is missing.
fn child_named(dir: &Path, name: &str) -> Result<Option<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(dir),
    };
    let folded = names::fold(name);
    let mut found = None;
    for entry in entries {
        let entry = entry.at(dir)?;
        let Ok(n) = entry.file_name().into_string() else {
            continue;
        };
        if n == name {
            return Ok(Some(n));
        }
        if names::fold(&n) == folded {
            found = Some(n);
        }
    }
    Ok(found)
}

/// The folder New Request files a request for `operation` into: named after the operation, or
/// `Port/Operation` when the WSDL has more than one supported port, so a typical WSDL gets one
/// level. A port counts when its binding has a supported operation, so a SOAP 1.1 port next
/// to a SOAP 1.2 or rpc/encoded one still gives the flat layout. A name that is no valid
/// folder name gives the top level, `""`.
pub fn operation_folder(wsdl: &Wsdl, operation: &OperationRef) -> String {
    let defs = &wsdl.definitions;
    let supported: Vec<_> = defs
        .services
        .iter()
        .flat_map(|s| &s.ports)
        .filter(|p| {
            defs.binding(&p.binding)
                .is_some_and(|b| b.operations.iter().any(|o| o.is_supported()))
        })
        .collect();
    let port = supported.iter().find(|p| p.binding == operation.binding);
    let folder = match port {
        Some(port) if supported.len() > 1 => names::join(&port.name, &operation.operation),
        _ => operation.operation.clone(),
    };
    if names::validate_folder_path(&folder).is_ok() {
        folder
    } else {
        String::new()
    }
}

const COLUMNS: &str = "id, file_name, operation, last_server_id, created_at, pinned_server_id,
                       pinned_parameter_set_id";

type RawRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

fn raw_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

fn to_meta(
    (id, file_name, operation, last_server, created_at, pinned_server, pinned_set): RawRow,
) -> Result<RequestMeta> {
    let (folder, file) = names::split(&file_name);
    let name = names::stem_of(file)
        .ok_or_else(|| ProjectError::Corrupt(format!("bad request file name {file_name:?}")))?;
    Ok(RequestMeta {
        id: RequestId(parse_uuid(&id, "request")?),
        name: name.to_owned(),
        folder: folder.to_owned(),
        // A hint: a stored value that no longer parses reads as no operation.
        operation: operation.as_deref().and_then(|s| s.parse().ok()),
        last_server: last_server
            .as_deref()
            .map(|s| parse_uuid(s, "server").map(ServerId))
            .transpose()?,
        created_at: parse_time(&created_at)?,
        pinned_server: pinned_server
            .as_deref()
            .map(|s| parse_uuid(s, "server").map(ServerId))
            .transpose()?,
        pinned_parameter_set: pinned_set
            .as_deref()
            .map(|s| parse_uuid(s, "parameter set").map(ParameterSetId))
            .transpose()?,
    })
}

impl Project {
    /// All requests in sidebar order.
    pub fn requests(&self) -> Result<Vec<RequestMeta>> {
        let rows: Vec<RawRow> = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM request ORDER BY sort_order, file_name"
            ))?;
            stmt.query_map([], raw_row)?
                .collect::<rusqlite::Result<_>>()?
        };
        rows.into_iter().map(to_meta).collect()
    }

    pub fn request(&self, id: RequestId) -> Result<RequestMeta> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM request WHERE id = ?1"),
                params![id.to_string()],
                raw_row,
            )
            .optional()?
            .ok_or(ProjectError::UnknownRequest(id))?;
        to_meta(row)
    }

    /// Absolute path of the request's file.
    pub fn request_path(&self, id: RequestId) -> Result<PathBuf> {
        Ok(join_rel(&self.requests_dir(), &self.file_name(id)?))
    }

    /// The `requests/` folder, which holds the request files and their folders.
    pub fn requests_dir(&self) -> PathBuf {
        self.root.join(REQUESTS_DIR)
    }

    fn file_name(&self, id: RequestId) -> Result<String> {
        self.conn
            .query_row(
                "SELECT file_name FROM request WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(ProjectError::UnknownRequest(id))
    }

    /// Folded names in use, from the database and from files on disk (a file may have
    /// appeared since the last reconciliation).
    fn taken_names(&self) -> Result<HashSet<String>> {
        let mut taken = HashSet::new();
        for r in self.requests()? {
            taken.insert(names::fold(&r.name));
        }
        for file in walk(&self.requests_dir())?.files {
            if let Some(stem) = names::stem_of(names::split(&file).1) {
                taken.insert(names::fold(stem));
            }
        }
        Ok(taken)
    }

    fn check_new_name(&self, name: &str, taken: &HashSet<String>) -> Result<()> {
        names::validate_request_name(name).map_err(|reason| ProjectError::InvalidName {
            name: name.to_owned(),
            reason,
        })?;
        if taken.contains(&names::fold(name)) {
            return Err(ProjectError::NameTaken(name.to_owned()));
        }
        Ok(())
    }

    /// Creates a request for `operation` at the top level, named `<Operation> <n>` with the
    /// lowest free `n`, starting with the project's most recently used server.
    pub fn create_request(&mut self, operation: &OperationRef, text: &str) -> Result<RequestMeta> {
        self.create_request_in("", operation, text)
    }

    /// As [`create_request`](Self::create_request), in `folder` (created if missing, an
    /// existing one matched ignoring case). Names are unique across all folders.
    pub fn create_request_in(
        &mut self,
        folder: &str,
        operation: &OperationRef,
        text: &str,
    ) -> Result<RequestMeta> {
        let name = names::numbered(&operation.operation, &self.taken_names()?);
        self.create_request_named_in(folder, &name, Some(operation), text)
    }

    /// New Request, in the app and the CLI: `text` (usually
    /// [`ProjectSchema::request_template`](super::ProjectSchema::request_template)) goes into
    /// `folder`, else into the operation's folder ([`operation_folder`]), named `name`, else
    /// `<Operation> <n>`.
    pub fn create_request_for(
        &mut self,
        wsdl: &Wsdl,
        operation: &OperationRef,
        folder: Option<&str>,
        name: Option<&str>,
        text: &str,
    ) -> Result<RequestMeta> {
        let folder = folder.map_or_else(|| operation_folder(wsdl, operation), str::to_owned);
        match name {
            Some(name) => self.create_request_named_in(&folder, name, Some(operation), text),
            None => self.create_request_in(&folder, operation, text),
        }
    }

    /// Creates a request with a given name at the top level. The file is written as UTF-8
    /// without BOM.
    pub fn create_request_named(
        &mut self,
        name: &str,
        operation: Option<&OperationRef>,
        text: &str,
    ) -> Result<RequestMeta> {
        self.create_request_named_in("", name, operation, text)
    }

    /// As [`create_request_named`](Self::create_request_named), in `folder` (created if
    /// missing, an existing one matched ignoring case).
    pub fn create_request_named_in(
        &mut self,
        folder: &str,
        name: &str,
        operation: Option<&OperationRef>,
        text: &str,
    ) -> Result<RequestMeta> {
        self.check_new_name(name, &self.taken_names()?)?;
        let folder = self.ensure_folder(folder)?;
        let new = NewRequest {
            last_server: self.active_server()?,
            ..NewRequest::default()
        };
        self.insert_request(&folder, name, operation, new, text.as_bytes(), None)
    }

    /// Writes the file, then the row; removes the file again if the row fails.
    /// `after` places the new request right after that one; otherwise it is appended.
    fn insert_request(
        &mut self,
        folder: &str,
        name: &str,
        operation: Option<&OperationRef>,
        new: NewRequest,
        bytes: &[u8],
        after: Option<RequestId>,
    ) -> Result<RequestMeta> {
        let file_name = names::join(folder, &names::file_name(name));
        let path = join_rel(&self.requests_dir(), &file_name);
        atomic_write(&path, bytes)?;
        let id = RequestId::new();
        let created_at = SystemTime::now();
        let inserted = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            let order: i64 = match after {
                Some(a) => {
                    let o: i64 = tx.query_row(
                        "SELECT sort_order FROM request WHERE id = ?1",
                        params![a.to_string()],
                        |r| r.get(0),
                    )?;
                    tx.execute(
                        "UPDATE request SET sort_order = sort_order + 1 WHERE sort_order > ?1",
                        params![o],
                    )?;
                    o + 1
                }
                None => tx.query_row(
                    "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM request",
                    [],
                    |r| r.get(0),
                )?,
            };
            tx.execute(
                "INSERT INTO request (id, file_name, operation, last_server_id, created_at,
                                      sort_order, pinned_server_id, pinned_parameter_set_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id.to_string(),
                    file_name,
                    operation.map(OperationRef::to_string),
                    new.last_server.map(|s| s.to_string()),
                    timefmt::format(created_at),
                    order,
                    new.pinned_server.map(|s| s.to_string()),
                    new.pinned_parameter_set.map(|s| s.to_string())
                ],
            )?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(e) = inserted {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        self.request(id)
    }

    /// Renames the request and its file. Changing only the letter case is allowed.
    pub fn rename_request(&mut self, id: RequestId, new_name: &str) -> Result<RequestMeta> {
        let old = self.request(id)?;
        if old.name == new_name {
            return Ok(old);
        }
        let mut taken = self.taken_names()?;
        taken.remove(&names::fold(&old.name));
        self.check_new_name(new_name, &taken)?;

        let dir = self.requests_dir();
        let old_path = join_rel(&dir, &self.file_name(id)?);
        let new_file = names::join(&old.folder, &names::file_name(new_name));
        let new_path = join_rel(&dir, &new_file);
        self.rename_with_rows(&[(id.to_string(), new_file)], &old_path, &new_path)?;
        self.request(id)
    }

    /// Copies the request (file bytes unchanged, so a BOM survives) as `<name> copy`,
    /// `<name> copy 2`, …, in the original's folder and right after it, with its pins. History
    /// is not copied.
    pub fn duplicate_request(&mut self, id: RequestId) -> Result<RequestMeta> {
        let src = self.request(id)?;
        let path = self.request_path(id)?;
        let bytes = fs::read(&path).at(&path)?;
        let name = names::copy_name(&src.name, &self.taken_names()?);
        self.check_new_name(&name, &HashSet::new())?;
        self.insert_request(
            &src.folder,
            &name,
            src.operation.as_ref(),
            NewRequest {
                last_server: src.last_server,
                pinned_server: src.pinned_server,
                pinned_parameter_set: src.pinned_parameter_set,
            },
            &bytes,
            Some(id),
        )
    }

    /// Deletes the request, its file, and its history (rows and files).
    pub fn delete_request(&mut self, id: RequestId) -> Result<()> {
        let path = self.request_path(id)?;
        remove_file_if_exists(&path)?;
        // History rows go with the request row (ON DELETE CASCADE).
        self.conn
            .execute("DELETE FROM request WHERE id = ?1", params![id.to_string()])?;
        remove_dir_if_exists(&self.root.join(HISTORY_DIR).join(id.to_string()))
    }

    /// The request's XML, decoded (BOM removed, UTF-16/Latin-1 converted).
    pub fn read_request(&self, id: RequestId) -> Result<String> {
        let path = self.request_path(id)?;
        let bytes = fs::read(&path).at(&path)?;
        xml::decode(&bytes)
            .map(|d| d.text)
            .map_err(|source| ProjectError::Decode { path, source })
    }

    /// Saves the request's XML atomically as UTF-8, keeping a UTF-8 BOM if the file had one.
    ///
    /// A file that was UTF-16 or Latin-1 becomes UTF-8; an `encoding` in its XML declaration
    /// is not rewritten (the editor owns the text).
    pub fn write_request(&self, id: RequestId, text: &str) -> Result<()> {
        let path = self.request_path(id)?;
        let had_bom = match fs::File::open(&path) {
            Ok(f) => {
                let mut head = Vec::with_capacity(3);
                f.take(3).read_to_end(&mut head).at(&path)?;
                head == xml::UTF8_BOM
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e).at(&path),
        };
        atomic_write(&path, &xml::encode_utf8(text, had_bom))
    }

    /// Moves the request's file into `folder` (created if missing, an existing one matched
    /// ignoring case; `""` is the top level). Same id, same history.
    pub fn move_request(&mut self, id: RequestId, folder: &str) -> Result<RequestMeta> {
        self.check_move_request(id, folder)?;
        let meta = self.request(id)?;
        if self.resolve_folder(folder)? == meta.folder {
            return Ok(meta);
        }
        let folder = self.ensure_folder(folder)?;
        let dir = self.requests_dir();
        let old_file = self.file_name(id)?;
        let new_file = names::join(&folder, names::split(&old_file).1);
        let (old_path, new_path) = (join_rel(&dir, &old_file), join_rel(&dir, &new_file));
        self.rename_with_rows(&[(id.to_string(), new_file)], &old_path, &new_path)?;
        self.request(id)
    }

    /// Whether [`move_request`](Self::move_request) would move the request: `folder` must be
    /// a valid path and hold no file of the request's name (one from Finder or git: names are
    /// unique across folders otherwise). Moving to its own folder is fine and does nothing.
    pub fn check_move_request(&self, id: RequestId, folder: &str) -> Result<()> {
        check_folder_path(folder)?;
        let meta = self.request(id)?;
        let resolved = self.resolve_folder(folder)?;
        if resolved == meta.folder {
            return Ok(());
        }
        let file = names::file_name(&meta.name);
        if child_named(&join_rel(&self.requests_dir(), &resolved), &file)?.is_some() {
            return Err(ProjectError::NameTaken(meta.name));
        }
        Ok(())
    }

    /// Every folder under `requests/`, empty ones included, `/`-separated and sorted. The
    /// disk is the truth for folders; the database does not know them.
    pub fn folders(&self) -> Result<Vec<String>> {
        Ok(walk(&self.requests_dir())?.folders)
    }

    /// Creates the folder `path` and any missing parents; an existing parent is matched
    /// ignoring case. Returns the path as created. Fails if the folder exists.
    pub fn create_folder(&mut self, path: &str) -> Result<String> {
        check_folder_path(path)?;
        if path.is_empty() || self.folder_exists(path)? {
            return Err(ProjectError::FolderExists(path.to_owned()));
        }
        self.ensure_folder(path)
    }

    /// New Folder: creates `New Folder` in `parent`, or `New Folder 2`, `New Folder 3`, … when
    /// that is taken. Returns its path.
    pub fn create_untitled_folder(&mut self, parent: &str) -> Result<String> {
        check_folder_path(parent)?;
        let parent = self.resolve_folder(parent)?;
        let dir = join_rel(&self.requests_dir(), &parent);
        let mut name = UNTITLED_FOLDER.to_owned();
        let mut n = 1u64;
        while child_named(&dir, &name)?.is_some() {
            n += 1;
            name = format!("{UNTITLED_FOLDER} {n}");
        }
        self.ensure_folder(&names::join(&parent, &name))
    }

    /// Renames the folder at `path` (its last component); requests inside keep their ids and
    /// history. Changing only the letter case is allowed. Returns the new path.
    pub fn rename_folder(&mut self, path: &str, new_name: &str) -> Result<String> {
        names::validate_request_name(new_name).map_err(|reason| ProjectError::InvalidName {
            name: new_name.to_owned(),
            reason,
        })?;
        self.existing_folder(path)?;
        let (parent, old_name) = names::split(path);
        if old_name == new_name {
            return Ok(path.to_owned());
        }
        let new_path = names::join(parent, new_name);
        if names::fold(old_name) != names::fold(new_name) && self.folder_exists(&new_path)? {
            return Err(ProjectError::FolderExists(new_path));
        }
        self.move_tree(path, &new_path)?;
        Ok(new_path)
    }

    /// Moves the folder at `path` into `parent` (`""` is the top level). Returns its new path.
    /// Refused into itself or a folder inside it, and when `parent` already has a folder of
    /// that name.
    pub fn move_folder(&mut self, path: &str, parent: &str) -> Result<String> {
        self.check_move_folder(path, parent)?;
        let parent = self.resolve_folder(parent)?;
        let (from_parent, name) = names::split(path);
        if from_parent == parent {
            return Ok(path.to_owned());
        }
        let to = names::join(&parent, name);
        self.move_tree(path, &to)?;
        Ok(to)
    }

    /// Whether [`move_folder`](Self::move_folder) would do it; moving into its own parent is
    /// fine and does nothing.
    pub fn check_move_folder(&self, path: &str, parent: &str) -> Result<()> {
        self.existing_folder(path)?;
        if !parent.is_empty() {
            self.existing_folder(parent)?;
        }
        let parent = self.resolve_folder(parent)?;
        let path_folded = names::fold(path);
        if names::is_within(&names::fold(&parent), &path_folded) {
            return Err(ProjectError::MoveIntoItself(path.to_owned()));
        }
        let (from_parent, name) = names::split(path);
        if from_parent != parent && self.folder_exists(&names::join(&parent, name))? {
            return Err(ProjectError::FolderExists(names::join(&parent, name)));
        }
        Ok(())
    }

    /// Deletes the folder with everything in it: its requests and their history, its
    /// folders, and any other files it holds (as the Finder would). Returns the requests
    /// that were deleted.
    pub fn delete_folder(&mut self, path: &str) -> Result<Vec<RequestId>> {
        self.existing_folder(path)?;
        let inside: Vec<RequestId> = self
            .requests()?
            .into_iter()
            .filter(|r| names::is_within(&r.folder, path))
            .map(|r| r.id)
            .collect();
        for id in &inside {
            self.delete_request(*id)?;
        }
        remove_dir_if_exists(&join_rel(&self.requests_dir(), path))?;
        Ok(inside)
    }

    /// `path` spelled the way the folders on disk spell it, compared ignoring case component
    /// by component; components that don't exist yet stay as given.
    fn resolve_folder(&self, path: &str) -> Result<String> {
        if path.is_empty() {
            return Ok(String::new());
        }
        let dir = self.requests_dir();
        let mut resolved = String::new();
        let mut on_disk = true;
        for component in path.split('/') {
            let found = if on_disk {
                child_named(&join_rel(&dir, &resolved), component)?
                    .filter(|n| join_rel(&dir, &names::join(&resolved, n)).is_dir())
            } else {
                None
            };
            on_disk = found.is_some();
            resolved = names::join(&resolved, found.as_deref().unwrap_or(component));
        }
        Ok(resolved)
    }

    fn folder_exists(&self, path: &str) -> Result<bool> {
        let resolved = self.resolve_folder(path)?;
        Ok(join_rel(&self.requests_dir(), &resolved).is_dir())
    }

    /// Checks that `path` names a folder (not the top level) exactly as it is spelled on disk.
    fn existing_folder(&self, path: &str) -> Result<()> {
        if path.is_empty() || !join_rel(&self.requests_dir(), path).is_dir() {
            return Err(ProjectError::UnknownFolder(path.to_owned()));
        }
        Ok(())
    }

    /// Creates `path` and its missing parents, matching existing ones ignoring case; returns
    /// the path as it is now on disk.
    fn ensure_folder(&mut self, path: &str) -> Result<String> {
        check_folder_path(path)?;
        let resolved = self.resolve_folder(path)?;
        let dir = join_rel(&self.requests_dir(), &resolved);
        fs::create_dir_all(&dir).at(&dir)?;
        Ok(resolved)
    }

    /// Renames the folder `from` to `to` on disk and every row inside it in the database, in
    /// one transaction that is only committed once the rename worked.
    fn move_tree(&mut self, from: &str, to: &str) -> Result<()> {
        let dir = self.requests_dir();
        let rows: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, file_name FROM request")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        // A file and a folder can't share a path, so every match is strictly inside `from`.
        let moved: Vec<(String, String)> = rows
            .into_iter()
            .filter_map(|(id, file)| Some((id, names::rebase(&file, from, to)?)))
            .collect();
        self.rename_with_rows(&moved, &join_rel(&dir, from), &join_rel(&dir, to))
    }

    /// Points each request in `rows` (id, new file name) at its new file and renames `from` to
    /// `to` on disk, in one transaction that is only committed once the rename worked. A failed
    /// commit renames back.
    fn rename_with_rows(
        &mut self,
        rows: &[(String, String)],
        from: &Path,
        to: &Path,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (id, file) in rows {
            tx.execute(
                "UPDATE request SET file_name = ?1 WHERE id = ?2",
                params![file, id],
            )?;
        }
        fs::rename(from, to).at(from)?;
        if let Err(e) = tx.commit() {
            let _ = fs::rename(to, from);
            return Err(e.into());
        }
        Ok(())
    }

    /// Sets the sidebar order. Requests not listed keep their relative order after the
    /// listed ones; unknown ids are ignored.
    pub fn set_request_order(&mut self, order: &[RequestId]) -> Result<()> {
        let current: Vec<RequestId> = self.requests()?.into_iter().map(|r| r.id).collect();
        let known: HashSet<RequestId> = current.iter().copied().collect();
        let mut seen = HashSet::new();
        let full: Vec<RequestId> = order
            .iter()
            .copied()
            .filter(|id| known.contains(id) && seen.insert(*id))
            .chain(current.iter().copied().filter(|id| !order.contains(id)))
            .collect();
        let tx = self.conn.transaction()?;
        for (i, id) in full.iter().enumerate() {
            tx.execute(
                "UPDATE request SET sort_order = ?1 WHERE id = ?2",
                params![i as i64, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn check_folder_path(path: &str) -> Result<()> {
    names::validate_folder_path(path).map_err(|reason| ProjectError::InvalidName {
        name: path.to_owned(),
        reason,
    })
}
