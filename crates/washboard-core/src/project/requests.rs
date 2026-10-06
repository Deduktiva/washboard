//! Requests: rows in `request`, XML in `requests/<name>.xml`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use super::fsutil::{IoContext, atomic_write, remove_dir_if_exists, remove_file_if_exists};
use super::names::{self, REQUEST_EXT};
use super::{
    HISTORY_DIR, LAST_SERVER_KEY, Project, ProjectError, REQUESTS_DIR, Result, parse_time,
    parse_uuid, timefmt,
};
use crate::model::{OperationRef, QName, RequestId, RequestMeta, ServerId};
use crate::xml;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

const COLUMNS: &str = "id, file_name, operation, last_server_id, created_at";

type RawRow = (String, String, Option<String>, Option<String>, String);

fn raw_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn to_meta((id, file_name, operation, last_server, created_at): RawRow) -> Result<RequestMeta> {
    let name = names::stem_of(&file_name)
        .ok_or_else(|| ProjectError::Corrupt(format!("bad request file name {file_name:?}")))?;
    Ok(RequestMeta {
        id: RequestId(parse_uuid(&id, "request")?),
        name: name.to_owned(),
        operation: operation.as_deref().and_then(decode_operation),
        last_server: last_server
            .as_deref()
            .map(|s| parse_uuid(s, "server").map(ServerId))
            .transpose()?,
        created_at: parse_time(&created_at)?,
    })
}

/// `{ns}Binding#Operation`. Stored as a hint; a value that no longer parses reads as `None`.
fn encode_operation(op: &OperationRef) -> String {
    format!("{}#{}", op.binding, op.operation)
}

fn decode_operation(s: &str) -> Option<OperationRef> {
    let (binding, operation) = s.rsplit_once('#')?;
    let binding = match binding.strip_prefix('{') {
        Some(rest) => {
            let (ns, local) = rest.split_once('}')?;
            QName::new(ns, local)
        }
        None => QName::new("", binding),
    };
    if binding.local.is_empty() || operation.is_empty() {
        return None;
    }
    Some(OperationRef {
        binding,
        operation: operation.to_owned(),
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
        Ok(self.root.join(REQUESTS_DIR).join(self.file_name(id)?))
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
        let dir = self.root.join(REQUESTS_DIR);
        for entry in fs::read_dir(&dir).at(&dir)? {
            let entry = entry.at(&dir)?;
            if let Some(stem) = entry.file_name().to_str().and_then(names::stem_of) {
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

    /// Creates a request for `operation`, named `<Operation> <n>` with the lowest free `n`,
    /// starting with the project's most recently used server.
    pub fn create_request(&mut self, operation: &OperationRef, text: &str) -> Result<RequestMeta> {
        let name = names::numbered(&operation.operation, &self.taken_names()?);
        self.create_request_named(&name, Some(operation), text)
    }

    /// Creates a request with a given name. The file is written as UTF-8 without BOM.
    pub fn create_request_named(
        &mut self,
        name: &str,
        operation: Option<&OperationRef>,
        text: &str,
    ) -> Result<RequestMeta> {
        self.check_new_name(name, &self.taken_names()?)?;
        let last_server = self.last_used_server()?;
        self.insert_request(name, operation, last_server, text.as_bytes(), None)
    }

    /// Writes the file, then the row; removes the file again if the row fails.
    /// `after` places the new request right after that one; otherwise it is appended.
    fn insert_request(
        &mut self,
        name: &str,
        operation: Option<&OperationRef>,
        last_server: Option<ServerId>,
        bytes: &[u8],
        after: Option<RequestId>,
    ) -> Result<RequestMeta> {
        let file_name = format!("{name}{REQUEST_EXT}");
        let path = self.root.join(REQUESTS_DIR).join(&file_name);
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
                                      sort_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.to_string(),
                    file_name,
                    operation.map(encode_operation),
                    last_server.map(|s| s.to_string()),
                    timefmt::format(created_at),
                    order
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

        let dir = self.root.join(REQUESTS_DIR);
        let old_path = dir.join(self.file_name(id)?);
        let new_file = format!("{new_name}{REQUEST_EXT}");
        let new_path = dir.join(&new_file);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE request SET file_name = ?1 WHERE id = ?2",
            params![new_file, id.to_string()],
        )?;
        fs::rename(&old_path, &new_path).at(&old_path)?;
        if let Err(e) = tx.commit() {
            let _ = fs::rename(&new_path, &old_path);
            return Err(e.into());
        }
        self.request(id)
    }

    /// Copies the request (file bytes unchanged, so a BOM survives) as `<name> copy`,
    /// `<name> copy 2`, …, placed right after the original. History is not copied.
    pub fn duplicate_request(&mut self, id: RequestId) -> Result<RequestMeta> {
        let src = self.request(id)?;
        let path = self.request_path(id)?;
        let bytes = fs::read(&path).at(&path)?;
        let name = names::copy_name(&src.name, &self.taken_names()?);
        self.check_new_name(&name, &HashSet::new())?;
        self.insert_request(
            &name,
            src.operation.as_ref(),
            src.last_server,
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
                head == UTF8_BOM
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e).at(&path),
        };
        atomic_write(&path, &xml::encode_utf8(text, had_bom))
    }

    /// Remembers the server chosen for the request; also becomes the project's most recently
    /// used server, which new requests start with.
    pub fn set_request_last_server(
        &mut self,
        id: RequestId,
        server: Option<ServerId>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE request SET last_server_id = ?1 WHERE id = ?2",
            params![server.map(|s| s.to_string()), id.to_string()],
        )?;
        if n == 0 {
            return Err(ProjectError::UnknownRequest(id));
        }
        if let Some(s) = server {
            self.set_ui_state(LAST_SERVER_KEY, Some(&s.to_string()))?;
        }
        Ok(())
    }

    /// The most recently used server, if it still exists.
    pub fn last_used_server(&self) -> Result<Option<ServerId>> {
        let Some(s) = self.ui_state(LAST_SERVER_KEY)? else {
            return Ok(None);
        };
        let Ok(u) = uuid::Uuid::parse_str(&s) else {
            return Ok(None);
        };
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM server WHERE id = ?1",
                params![u.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(exists.map(|_| ServerId(u)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_round_trip() {
        let op = OperationRef {
            binding: QName::new("urn:x#frag", "Binding"),
            operation: "GetCustomer".into(),
        };
        assert_eq!(decode_operation(&encode_operation(&op)), Some(op));
        let local = OperationRef {
            binding: QName::new("", "B"),
            operation: "Op".into(),
        };
        assert_eq!(decode_operation(&encode_operation(&local)), Some(local));
        assert_eq!(decode_operation("garbage"), None);
        assert_eq!(decode_operation("{urn:x#Op"), None);
    }
}
