//! Servers of a project. Passwords never touch the database; they go through a
//! [`SecretStore`] keyed by project and server id.
//!
//! Every server written is checked ([`Project::check_server`]): a name that is not blank and
//! not used by another server, and a URL [`http::send`](crate::http::send) accepts. Servers
//! are read without the check, so a project saved by an older version that holds a blank name
//! or a bad URL still opens; such a server can be fixed or deleted, but not saved unchanged.

use std::collections::HashSet;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::{ACTIVE_SERVER_KEY, Project, ProjectError, Result, names, parse_uuid};
use crate::http;
use crate::model::{Auth, Server, ServerId};
use crate::secrets::{SecretKey, SecretStore};

/// Why a server cannot be saved. The messages are meant for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerError {
    #[error("server name may not be empty")]
    EmptyName,
    /// Names address servers on the command line and in the toolbar's popup. Compared after
    /// trimming and case-insensitively, so "Prod" and "prod" cannot both exist; a server may
    /// change the case of its own name.
    #[error("a server named {0:?} already exists")]
    NameTaken(String),
    #[error("server URL may not be empty")]
    EmptyUrl,
    /// The reason, as sending would report it.
    #[error("{0}")]
    InvalidUrl(String),
}

const COLUMNS: &str = "id, name, url, ignore_tls_errors, auth_kind, username, timeout_secs";

type RawRow = (String, String, String, bool, String, Option<String>, i64);

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

fn to_server(
    (id, name, url, ignore_tls_errors, auth_kind, username, timeout): RawRow,
) -> Result<Server> {
    let auth = match auth_kind.as_str() {
        "none" => Auth::None,
        "basic" => Auth::Basic {
            username: username.unwrap_or_default(),
        },
        other => {
            return Err(ProjectError::Corrupt(format!(
                "unknown auth kind {other:?}"
            )));
        }
    };
    let timeout = u64::try_from(timeout)
        .map_err(|_| ProjectError::Corrupt(format!("negative timeout {timeout}")))?;
    Ok(Server {
        id: ServerId(parse_uuid(&id, "server")?),
        name,
        url,
        ignore_tls_errors,
        auth,
        timeout: Duration::from_secs(timeout),
    })
}

/// Whole seconds, rounded up so a sub-second timeout does not become "no time at all".
fn timeout_secs(d: Duration) -> i64 {
    let secs = d.as_secs().saturating_add(u64::from(d.subsec_nanos() > 0));
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn auth_columns(auth: &Auth) -> (&'static str, Option<&str>) {
    match auth {
        Auth::None => ("none", None),
        Auth::Basic { username } => ("basic", Some(username.as_str())),
    }
}

impl Project {
    /// The key under which the server's password is stored.
    pub fn secret_key(&self, server: ServerId) -> SecretKey {
        SecretKey {
            project: self.id,
            server,
        }
    }

    /// All servers in display order.
    pub fn servers(&self) -> Result<Vec<Server>> {
        let rows: Vec<RawRow> = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM server ORDER BY sort_order, name"
            ))?;
            stmt.query_map([], raw_row)?
                .collect::<rusqlite::Result<_>>()?
        };
        rows.into_iter().map(to_server).collect()
    }

    pub fn server(&self, id: ServerId) -> Result<Server> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM server WHERE id = ?1"),
                params![id.to_string()],
                raw_row,
            )
            .optional()?
            .ok_or(ProjectError::UnknownServer(id))?;
        to_server(row)
    }

    /// Whether `server` may be saved as it is: see [`ServerError`]. Its own id is not a name
    /// clash, so this serves both adding and updating. [`add_server`](Self::add_server) and
    /// [`update_server`](Self::update_server) check it themselves; front ends call it to
    /// refuse a server before asking for anything else, such as a password.
    ///
    /// Names are compared trimmed but stored as given; front ends trim them.
    pub fn check_server(&self, server: &Server) -> Result<()> {
        let name = server.name.trim();
        if name.is_empty() {
            return Err(ServerError::EmptyName.into());
        }
        if self
            .servers()?
            .iter()
            .any(|s| s.id != server.id && names::fold(s.name.trim()) == names::fold(name))
        {
            return Err(ServerError::NameTaken(name.to_owned()).into());
        }
        if server.url.trim().is_empty() {
            return Err(ServerError::EmptyUrl.into());
        }
        http::check_url(&server.url).map_err(ServerError::InvalidUrl)?;
        Ok(())
    }

    /// Adds a server at the end of the list. The caller picks the id (`ServerId::new()`).
    /// The timeout is stored in whole seconds. Refused unless
    /// [`check_server`](Self::check_server) accepts it.
    pub fn add_server(&mut self, server: &Server) -> Result<()> {
        self.check_server(server)?;
        let (kind, username) = auth_columns(&server.auth);
        self.conn.execute(
            "INSERT INTO server (id, name, url, ignore_tls_errors, auth_kind, username,
                                 timeout_secs, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                     (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM server))",
            params![
                server.id.to_string(),
                server.name,
                server.url,
                server.ignore_tls_errors,
                kind,
                username,
                timeout_secs(server.timeout)
            ],
        )?;
        Ok(())
    }

    /// Updates all fields of an existing server. Switching to [`Auth::None`] deletes the
    /// stored password. Refused unless [`check_server`](Self::check_server) accepts it.
    pub fn update_server(&mut self, server: &Server, secrets: &dyn SecretStore) -> Result<()> {
        self.check_server(server)?;
        let (kind, username) = auth_columns(&server.auth);
        let n = self.conn.execute(
            "UPDATE server SET name = ?2, url = ?3, ignore_tls_errors = ?4, auth_kind = ?5,
                               username = ?6, timeout_secs = ?7
             WHERE id = ?1",
            params![
                server.id.to_string(),
                server.name,
                server.url,
                server.ignore_tls_errors,
                kind,
                username,
                timeout_secs(server.timeout)
            ],
        )?;
        if n == 0 {
            return Err(ProjectError::UnknownServer(server.id));
        }
        if server.auth == Auth::None {
            secrets.delete(&self.secret_key(server.id))?;
        }
        Ok(())
    }

    /// Deletes the server and its password. Requests pinned to it or that used it last lose
    /// that setting (`ON DELETE SET NULL`), and it stops being the project's active server;
    /// history entries keep the id as a plain value.
    ///
    /// The password is deleted first: if the secret store fails, nothing has changed and the
    /// user can retry, instead of leaving an orphaned Keychain item.
    pub fn delete_server(&mut self, id: ServerId, secrets: &dyn SecretStore) -> Result<()> {
        self.server(id)?;
        secrets.delete(&self.secret_key(id))?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM server WHERE id = ?1", params![id.to_string()])?;
        tx.execute(
            "DELETE FROM ui_state WHERE key = ?1 AND value = ?2",
            params![ACTIVE_SERVER_KEY, id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets the display order. Servers not listed keep their relative order after the listed
    /// ones; unknown ids are ignored.
    pub fn set_server_order(&mut self, order: &[ServerId]) -> Result<()> {
        let current: Vec<ServerId> = self.servers()?.into_iter().map(|s| s.id).collect();
        let known: HashSet<ServerId> = current.iter().copied().collect();
        let mut seen = HashSet::new();
        let full: Vec<ServerId> = order
            .iter()
            .copied()
            .filter(|id| known.contains(id) && seen.insert(*id))
            .chain(current.iter().copied().filter(|id| !order.contains(id)))
            .collect();
        let tx = self.conn.transaction()?;
        for (i, id) in full.iter().enumerate() {
            tx.execute(
                "UPDATE server SET sort_order = ?1 WHERE id = ?2",
                params![i as i64, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The basic-auth password, if one is stored.
    pub fn server_password(
        &self,
        id: ServerId,
        secrets: &dyn SecretStore,
    ) -> Result<Option<String>> {
        self.server(id)?;
        Ok(secrets.get(&self.secret_key(id))?)
    }

    /// Stores (`Some`) or removes (`None`) the basic-auth password.
    pub fn set_server_password(
        &self,
        id: ServerId,
        password: Option<&str>,
        secrets: &dyn SecretStore,
    ) -> Result<()> {
        self.server(id)?;
        let key = self.secret_key(id);
        match password {
            Some(p) => secrets.set(&key, p)?,
            None => secrets.delete(&key)?,
        }
        Ok(())
    }
}
