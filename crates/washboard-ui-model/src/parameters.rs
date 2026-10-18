//! Parameter sets, and choosing a server and set for the project or pinning one to a request
//! (`docs/specs/parameters.md`).
//!
//! The toolbar's popups change the project's choice, or the selected request's pin when it has
//! one. Anything that changes what the open request's placeholders resolve to re-validates it.

use std::collections::BTreeMap;

use washboard_core::model::{ParameterSet, ParameterSetId, ServerId};

use crate::app::{App, ModelError, PendingDialog, ProjectKey};
use crate::diagnostics::Check;
use crate::event::Event;

/// What a set added from the Parameters pane is called, numbered from 2 when taken.
const NEW_SET_NAME: &str = "New Set";

impl App {
    /// The toolbar's server popup: changes the selected request's pin when it has one,
    /// otherwise the project's active server.
    pub fn choose_server(&mut self, key: ProjectKey, server: ServerId) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let sel = window.selection();
        match window.selected_request() {
            Some(request) if sel.server_pinned => {
                window.project.pin_server(request, Some(server))?;
            }
            _ => window.project.set_active_server(Some(server))?,
        }
        self.events
            .push(Event::ServerSelectionChanged { project: key });
        Ok(())
    }

    /// The pin button beside the server popup: pins the server shown now, or unpins.
    pub fn set_server_pinned(&mut self, key: ProjectKey, pinned: bool) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let Some(request) = window.selected_request() else {
            return Err(ModelError::NoRequestSelected);
        };
        let server = if pinned {
            Some(window.selection().server.ok_or(ModelError::NoServer)?)
        } else {
            None
        };
        window.project.pin_server(request, server)?;
        self.events
            .push(Event::ServerSelectionChanged { project: key });
        Ok(())
    }

    /// The toolbar's parameter set popup. Like the server popup it has no "none" item: while a
    /// project has sets, one is always used. Changes the selected request's pin when it has
    /// one, otherwise the project's active set.
    pub fn choose_parameter_set(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let sel = window.selection();
        match window.selected_request() {
            Some(request) if sel.parameter_set_pinned => {
                window.project.pin_parameter_set(request, Some(set))?;
            }
            _ => window.project.set_active_parameter_set(Some(set))?,
        }
        self.parameter_selection_changed(key);
        Ok(())
    }

    /// The pin button beside the set popup: pins the set shown now, or unpins. Without a set
    /// there is nothing to pin.
    pub fn set_parameter_set_pinned(
        &mut self,
        key: ProjectKey,
        pinned: bool,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let Some(request) = window.selected_request() else {
            return Err(ModelError::NoRequestSelected);
        };
        let set = if pinned {
            let Some(set) = window.selection().parameter_set else {
                return Ok(());
            };
            Some(set)
        } else {
            None
        };
        window.project.pin_parameter_set(request, set)?;
        self.parameter_selection_changed(key);
        Ok(())
    }

    /// Settings ▸ Parameters: a new variable, unset in every set.
    pub fn add_parameter(&mut self, key: ProjectKey, name: &str) -> Result<(), ModelError> {
        self.window(key)?.project.add_parameter(name.trim())?;
        self.parameters_changed(key)
    }

    pub fn rename_parameter(
        &mut self,
        key: ProjectKey,
        old: &str,
        new: &str,
    ) -> Result<(), ModelError> {
        self.window(key)?
            .project
            .rename_parameter(old, new.trim())?;
        self.parameters_changed(key)
    }

    pub fn delete_parameter(&mut self, key: ProjectKey, name: &str) -> Result<(), ModelError> {
        self.window(key)?.project.delete_parameter(name)?;
        self.parameters_changed(key)
    }

    /// Delete for a variable: asks first when a set has a value for it, which goes with it.
    pub fn ask_delete_parameter(&mut self, key: ProjectKey, name: &str) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let used = window
            .parameter_sets
            .iter()
            .filter(|s| s.values.contains_key(name))
            .count();
        if used == 0 {
            return self.delete_parameter(key, name);
        }
        let sets = if used == 1 {
            "1 parameter set".to_owned()
        } else {
            format!("{used} parameter sets")
        };
        self.confirm_delete(
            PendingDialog::DeleteParameter {
                project: key,
                name: name.to_owned(),
            },
            format!("Delete the parameter “{name}”?"),
            &format!(
                "Its values in {sets} are deleted too. Requests that use it report it as \
                 undefined."
            ),
        );
        Ok(())
    }

    /// Settings ▸ Parameters ▸ Add Set: a set with no values, called “New Set” (numbered when
    /// taken), at the end.
    pub fn add_parameter_set(&mut self, key: ProjectKey) -> Result<ParameterSetId, ModelError> {
        let window = self.window(key)?;
        let set = ParameterSet {
            id: ParameterSetId::new(),
            name: free_set_name(&window.parameter_sets),
            values: BTreeMap::new(),
        };
        window.project.add_parameter_set(&set)?;
        self.parameters_changed(key)?;
        Ok(set.id)
    }

    /// Settings ▸ Parameters ▸ Duplicate Set: a copy with the same values, after the original.
    pub fn duplicate_parameter_set(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
    ) -> Result<ParameterSetId, ModelError> {
        let copy = self.window(key)?.project.duplicate_parameter_set(set)?;
        self.parameters_changed(key)?;
        Ok(copy.id)
    }

    pub fn rename_parameter_set(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
        name: &str,
    ) -> Result<(), ModelError> {
        self.window(key)?
            .project
            .rename_parameter_set(set, name.trim())?;
        self.parameters_changed(key)
    }

    pub fn delete_parameter_set(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
    ) -> Result<(), ModelError> {
        self.window(key)?.project.delete_parameter_set(set)?;
        self.parameters_changed(key)
    }

    /// Delete for a set: asks first when it has values, which go with it.
    pub fn ask_delete_parameter_set(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        let Some(found) = window.parameter_sets.iter().find(|s| s.id == set) else {
            return Err(washboard_core::project::ProjectError::UnknownParameterSet(set).into());
        };
        if found.values.is_empty() {
            return self.delete_parameter_set(key, set);
        }
        let title = format!("Delete the parameter set “{}”?", found.name);
        self.confirm_delete(
            PendingDialog::DeleteParameterSet { project: key, set },
            title,
            "Its values are deleted too. Requests pinned to it will use the project's set.",
        );
        Ok(())
    }

    /// The grid's columns after a drag; names it leaves out keep their place after these.
    pub fn set_parameter_order(
        &mut self,
        key: ProjectKey,
        order: &[String],
    ) -> Result<(), ModelError> {
        self.window(key)?.project.set_parameter_order(order)?;
        self.parameters_changed(key)
    }

    /// The grid's rows, and the set popup's items, in a new order.
    pub fn set_parameter_set_order(
        &mut self,
        key: ProjectKey,
        order: &[ParameterSetId],
    ) -> Result<(), ModelError> {
        self.window(key)?.project.set_parameter_set_order(order)?;
        self.parameters_changed(key)
    }

    /// One cell of the grid: `Some` stores the value (an empty one too), `None` unsets it.
    pub fn set_parameter_value(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
        name: &str,
        value: Option<&str>,
    ) -> Result<(), ModelError> {
        self.window(key)?
            .project
            .set_parameter_value(set, name, value)?;
        self.parameters_changed(key)
    }

    /// The end of editing a value cell with `text`: clearing a value unsets it, and a cell that
    /// was unset or empty stays as it was when left empty. Returns whether anything changed.
    pub fn edit_parameter_value(
        &mut self,
        key: ProjectKey,
        set: ParameterSetId,
        name: &str,
        text: &str,
    ) -> Result<bool, ModelError> {
        let window = self.window(key)?;
        let old = window
            .parameter_sets
            .iter()
            .find(|s| s.id == set)
            .and_then(|s| s.values.get(name))
            .map(String::as_str);
        let new = match (text, old) {
            ("", None | Some("")) => return Ok(false),
            ("", Some(_)) => None,
            (text, _) if Some(text) == old => return Ok(false),
            (text, _) => Some(text),
        };
        self.set_parameter_value(key, set, name, new)?;
        Ok(true)
    }

    /// Move Up (`-1`) and Move Down (`1`) for the parameter `name`; at either end it stays.
    pub fn move_parameter(
        &mut self,
        key: ProjectKey,
        name: &str,
        by: isize,
    ) -> Result<(), ModelError> {
        let mut order = self.window(key)?.project.parameters()?;
        let Some(from) = order.iter().position(|p| p == name) else {
            return Ok(());
        };
        let Some(to) = from.checked_add_signed(by).filter(|to| *to < order.len()) else {
            return Ok(());
        };
        order.swap(from, to);
        self.set_parameter_order(key, &order)
    }

    /// Reloads the variables and sets, and re-validates: any change can make the open
    /// request's placeholders resolve differently.
    fn parameters_changed(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        self.window(key)?.reload_parameters()?;
        self.events.push(Event::ParametersChanged { project: key });
        self.parameter_selection_changed(key);
        Ok(())
    }

    pub(crate) fn parameter_selection_changed(&mut self, key: ProjectKey) {
        self.events
            .push(Event::ParameterSetSelectionChanged { project: key });
        self.start_check(key, Check::Full);
    }
}

/// “New Set”, or the first of “New Set 2”, “New Set 3”, … no set is called.
fn free_set_name(sets: &[ParameterSet]) -> String {
    let taken = |name: &str| {
        sets.iter()
            .any(|s| s.name.trim().to_lowercase() == name.to_lowercase())
    };
    (1..)
        .map(|n| match n {
            1 => NEW_SET_NAME.to_owned(),
            n => format!("{NEW_SET_NAME} {n}"),
        })
        .find(|name| !taken(name))
        .unwrap_or_else(|| NEW_SET_NAME.to_owned())
}
