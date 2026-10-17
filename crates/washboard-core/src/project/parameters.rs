//! Parameter sets, per-request pins, and the project's active server and set
//! (`docs/specs/parameters.md`).
//!
//! The variables are the grid's columns (`parameter`), the sets its rows (`parameter_set`),
//! and `parameter_value` holds the cells; a missing cell is an unset value. Which server and
//! set a send uses is [`Project::selection`]: the request's pin, else the project's active
//! choice in `ui_state`.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{OptionalExtension, params};

use super::{
    ACTIVE_PARAMETER_SET_KEY, ACTIVE_SERVER_KEY, Project, ProjectError, Result, names, parse_uuid,
};
use crate::interpolate;
use crate::model::{ParameterSet, ParameterSetId, RequestId, RequestMeta, ServerId};

/// Why a variable or set cannot be saved. The messages are meant for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParameterError {
    #[error("invalid parameter name {0:?}: use letters, digits and _, not starting with a digit")]
    InvalidName(String),
    #[error("a parameter named {0:?} already exists")]
    NameTaken(String),
    #[error("no parameter named {0:?}")]
    UnknownName(String),
    #[error("parameter set name may not be empty")]
    EmptySetName,
    /// Set names address sets on the command line; compared like server names.
    #[error("a parameter set named {0:?} already exists")]
    SetNameTaken(String),
}

/// Which server and parameter set a request uses, and whether each comes from its pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    /// `None` only while the project has no servers.
    pub server: Option<ServerId>,
    pub server_pinned: bool,
    pub parameter_set: Option<ParameterSetId>,
    pub parameter_set_pinned: bool,
}

impl Project {
    /// The variables, in column order.
    pub fn parameters(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM parameter ORDER BY sort_order, name")?;
        let names = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(names)
    }

    /// Adds a variable as the last column, unset in every set.
    pub fn add_parameter(&mut self, name: &str) -> Result<()> {
        self.check_new_parameter(name)?;
        self.conn.execute(
            "INSERT INTO parameter (name, sort_order)
             VALUES (?1, (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM parameter))",
            params![name],
        )?;
        Ok(())
    }

    /// Renames a variable in every set (`ON UPDATE CASCADE`). Requests that use the old name
    /// are not rewritten; they report it as undefined.
    pub fn rename_parameter(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return self.check_parameter_exists(old);
        }
        self.check_parameter_exists(old)?;
        self.check_new_parameter(new)?;
        self.conn.execute(
            "UPDATE parameter SET name = ?2 WHERE name = ?1",
            params![old, new],
        )?;
        Ok(())
    }

    /// Deletes a variable and its value in every set.
    pub fn delete_parameter(&mut self, name: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM parameter WHERE name = ?1", params![name])?;
        if n == 0 {
            return Err(ParameterError::UnknownName(name.to_owned()).into());
        }
        Ok(())
    }

    /// Sets the column order. Variables not listed keep their relative order after the listed
    /// ones; unknown names are ignored.
    pub fn set_parameter_order(&mut self, order: &[String]) -> Result<()> {
        let current = self.parameters()?;
        let full = full_order(&current, order);
        let tx = self.conn.transaction()?;
        for (i, name) in full.iter().enumerate() {
            tx.execute(
                "UPDATE parameter SET sort_order = ?1 WHERE name = ?2",
                params![i as i64, name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every set with its values, in row order.
    pub fn parameter_sets(&self) -> Result<Vec<ParameterSet>> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, name FROM parameter_set ORDER BY sort_order, name")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut values: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT set_id, name, value FROM parameter_value")?;
            let cells = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for cell in cells {
                let (set, name, value) = cell?;
                values.entry(set).or_default().insert(name, value);
            }
        }
        rows.into_iter()
            .map(|(id, name)| {
                Ok(ParameterSet {
                    id: ParameterSetId(parse_uuid(&id, "parameter set")?),
                    name,
                    values: values.remove(&id).unwrap_or_default(),
                })
            })
            .collect()
    }

    pub fn parameter_set(&self, id: ParameterSetId) -> Result<ParameterSet> {
        self.parameter_sets()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or(ProjectError::UnknownParameterSet(id))
    }

    /// Adds a set as the last row. The caller picks the id (`ParameterSetId::new()`); values
    /// for variables the project does not have are refused.
    pub fn add_parameter_set(&mut self, set: &ParameterSet) -> Result<()> {
        self.check_set_name(set.id, &set.name)?;
        let known: HashSet<String> = self.parameters()?.into_iter().collect();
        if let Some(name) = set.values.keys().find(|n| !known.contains(*n)) {
            return Err(ParameterError::UnknownName(name.clone()).into());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO parameter_set (id, name, sort_order)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM parameter_set))",
            params![set.id.to_string(), set.name],
        )?;
        for (name, value) in &set.values {
            tx.execute(
                "INSERT INTO parameter_value (set_id, name, value) VALUES (?1, ?2, ?3)",
                params![set.id.to_string(), name, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Copies the set with all its values into `<name> copy` (numbered when taken), placed
    /// right after the original.
    pub fn duplicate_parameter_set(&mut self, id: ParameterSetId) -> Result<ParameterSet> {
        let sets = self.parameter_sets()?;
        let original = sets
            .iter()
            .find(|s| s.id == id)
            .ok_or(ProjectError::UnknownParameterSet(id))?;
        let taken: HashSet<String> = sets.iter().map(|s| names::fold(s.name.trim())).collect();
        let copy = ParameterSet {
            id: ParameterSetId::new(),
            name: names::copy_name(original.name.trim(), &taken),
            values: original.values.clone(),
        };
        self.add_parameter_set(&copy)?;
        let mut order: Vec<ParameterSetId> = sets.iter().map(|s| s.id).collect();
        let at = order
            .iter()
            .position(|s| *s == id)
            .map_or(order.len(), |i| i + 1);
        order.insert(at, copy.id);
        self.set_parameter_set_order(&order)?;
        Ok(copy)
    }

    pub fn rename_parameter_set(&mut self, id: ParameterSetId, name: &str) -> Result<()> {
        self.check_set_name(id, name)?;
        let n = self.conn.execute(
            "UPDATE parameter_set SET name = ?2 WHERE id = ?1",
            params![id.to_string(), name],
        )?;
        if n == 0 {
            return Err(ProjectError::UnknownParameterSet(id));
        }
        Ok(())
    }

    /// Deletes the set and its values. Pins on it are cleared (`ON DELETE SET NULL`), and so is
    /// the project's active set when it was this one; history keeps the id as a plain value.
    pub fn delete_parameter_set(&mut self, id: ParameterSetId) -> Result<()> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "DELETE FROM parameter_set WHERE id = ?1",
            params![id.to_string()],
        )?;
        if n == 0 {
            return Err(ProjectError::UnknownParameterSet(id));
        }
        tx.execute(
            "DELETE FROM ui_state WHERE key = ?1 AND value = ?2",
            params![ACTIVE_PARAMETER_SET_KEY, id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets the row order. Sets not listed keep their relative order after the listed ones;
    /// unknown ids are ignored.
    pub fn set_parameter_set_order(&mut self, order: &[ParameterSetId]) -> Result<()> {
        let current: Vec<ParameterSetId> =
            self.parameter_sets()?.into_iter().map(|s| s.id).collect();
        let full = full_order(&current, order);
        let tx = self.conn.transaction()?;
        for (i, id) in full.iter().enumerate() {
            tx.execute(
                "UPDATE parameter_set SET sort_order = ?1 WHERE id = ?2",
                params![i as i64, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sets one cell: `Some` stores the value (an empty string is a value), `None` unsets it.
    pub fn set_parameter_value(
        &mut self,
        set: ParameterSetId,
        name: &str,
        value: Option<&str>,
    ) -> Result<()> {
        self.parameter_set_exists(set)?;
        self.check_parameter_exists(name)?;
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO parameter_value (set_id, name, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT(set_id, name) DO UPDATE SET value = excluded.value",
                params![set.to_string(), name, v],
            )?,
            None => self.conn.execute(
                "DELETE FROM parameter_value WHERE set_id = ?1 AND name = ?2",
                params![set.to_string(), name],
            )?,
        };
        Ok(())
    }

    /// The project's active server, if it still exists.
    pub fn active_server(&self) -> Result<Option<ServerId>> {
        let Some(id) = self.ui_state_uuid(ACTIVE_SERVER_KEY)?.map(ServerId) else {
            return Ok(None);
        };
        Ok(self.row_exists("server", &id.to_string())?.then_some(id))
    }

    /// Chooses the server for every request without a server pin.
    pub fn set_active_server(&mut self, server: Option<ServerId>) -> Result<()> {
        if let Some(id) = server {
            self.server(id)?;
        }
        let value = server.map(|s| s.to_string());
        self.set_ui_state(ACTIVE_SERVER_KEY, value.as_deref())
    }

    /// The project's active parameter set, if it still exists.
    pub fn active_parameter_set(&self) -> Result<Option<ParameterSetId>> {
        let Some(id) = self
            .ui_state_uuid(ACTIVE_PARAMETER_SET_KEY)?
            .map(ParameterSetId)
        else {
            return Ok(None);
        };
        Ok(self
            .row_exists("parameter_set", &id.to_string())?
            .then_some(id))
    }

    /// Chooses the set for every request without a set pin; `None` is "No Parameter Set".
    pub fn set_active_parameter_set(&mut self, set: Option<ParameterSetId>) -> Result<()> {
        if let Some(id) = set {
            self.parameter_set_exists(id)?;
        }
        let value = set.map(|s| s.to_string());
        self.set_ui_state(ACTIVE_PARAMETER_SET_KEY, value.as_deref())
    }

    /// Pins the request to a server (`Some`) or returns it to the project's (`None`).
    pub fn pin_server(&mut self, request: RequestId, server: Option<ServerId>) -> Result<()> {
        if let Some(id) = server {
            self.server(id)?;
        }
        self.update_request_column("pinned_server_id", request, server.map(|s| s.to_string()))
    }

    /// Pins the request to a parameter set (`Some`) or returns it to the project's (`None`).
    pub fn pin_parameter_set(
        &mut self,
        request: RequestId,
        set: Option<ParameterSetId>,
    ) -> Result<()> {
        if let Some(id) = set {
            self.parameter_set_exists(id)?;
        }
        self.update_request_column(
            "pinned_parameter_set_id",
            request,
            set.map(|s| s.to_string()),
        )
    }

    /// What a send of `request` uses: each of server and set from the request's pin, else the
    /// project's active one; a server falls back to the first, a set to none. Without a
    /// request, the project's choice. Pins and active choices that no longer exist are skipped,
    /// so a stale `RequestMeta` is fine.
    pub fn selection(&self, request: Option<&RequestMeta>) -> Result<Selection> {
        let mut out = Selection::default();
        if let Some(id) = request.and_then(|r| r.pinned_server)
            && self.row_exists("server", &id.to_string())?
        {
            out.server = Some(id);
            out.server_pinned = true;
        } else {
            out.server = match self.active_server()? {
                Some(id) => Some(id),
                None => self.servers()?.first().map(|s| s.id),
            };
        }
        if let Some(id) = request.and_then(|r| r.pinned_parameter_set)
            && self.row_exists("parameter_set", &id.to_string())?
        {
            out.parameter_set = Some(id);
            out.parameter_set_pinned = true;
        } else {
            out.parameter_set = self.active_parameter_set()?;
        }
        Ok(out)
    }

    /// The values a send with `set` fills in: every variable, and the set's values.
    pub fn parameter_values(&self, set: Option<ParameterSetId>) -> Result<interpolate::Values> {
        Ok(interpolate::Values {
            variables: self.parameters()?,
            set: set.map(|id| self.parameter_set(id)).transpose()?,
        })
    }

    fn check_new_parameter(&self, name: &str) -> Result<()> {
        if !interpolate::is_user_name(name) {
            return Err(ParameterError::InvalidName(name.to_owned()).into());
        }
        if self.parameters()?.iter().any(|n| n == name) {
            return Err(ParameterError::NameTaken(name.to_owned()).into());
        }
        Ok(())
    }

    fn check_parameter_exists(&self, name: &str) -> Result<()> {
        if !self.row_exists_by("parameter", "name", name)? {
            return Err(ParameterError::UnknownName(name.to_owned()).into());
        }
        Ok(())
    }

    fn check_set_name(&self, id: ParameterSetId, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ParameterError::EmptySetName.into());
        }
        if self
            .parameter_sets()?
            .iter()
            .any(|s| s.id != id && names::fold(s.name.trim()) == names::fold(name))
        {
            return Err(ParameterError::SetNameTaken(name.to_owned()).into());
        }
        Ok(())
    }

    fn parameter_set_exists(&self, id: ParameterSetId) -> Result<()> {
        if !self.row_exists("parameter_set", &id.to_string())? {
            return Err(ProjectError::UnknownParameterSet(id));
        }
        Ok(())
    }

    fn row_exists(&self, table: &str, id: &str) -> Result<bool> {
        self.row_exists_by(table, "id", id)
    }

    fn row_exists_by(&self, table: &str, column: &str, value: &str) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                &format!("SELECT 1 FROM {table} WHERE {column} = ?1"),
                params![value],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    fn ui_state_uuid(&self, key: &str) -> Result<Option<uuid::Uuid>> {
        Ok(self
            .ui_state(key)?
            .and_then(|s| uuid::Uuid::parse_str(&s).ok()))
    }

    fn update_request_column(
        &mut self,
        column: &str,
        request: RequestId,
        value: Option<String>,
    ) -> Result<()> {
        let n = self.conn.execute(
            &format!("UPDATE request SET {column} = ?1 WHERE id = ?2"),
            params![value, request.to_string()],
        )?;
        if n == 0 {
            return Err(ProjectError::UnknownRequest(request));
        }
        Ok(())
    }
}

/// `order`'s known entries without repeats, then the rest of `current` in its order.
fn full_order<T: Clone + Eq + std::hash::Hash>(current: &[T], order: &[T]) -> Vec<T> {
    let known: HashSet<&T> = current.iter().collect();
    let mut seen = HashSet::new();
    order
        .iter()
        .filter(|x| known.contains(x) && seen.insert(*x))
        .chain(current.iter().filter(|x| !order.contains(x)))
        .cloned()
        .collect()
}
