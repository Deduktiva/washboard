//! `set list|add|rm|rename` and `param list|add|rm|rename|set|unset`: the project's parameter
//! sets and variables (`docs/specs/parameters.md`). Sets are addressed by name, like servers.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use washboard_core::model::{ParameterSet, ParameterSetId};

use crate::support;

/// One line per set: its name, then `variable=value` for every variable it has a value for.
/// `*` marks the set a send uses without `--set` or a pin: the active one, else the first.
pub fn set_list(dir: &Path) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let active = project.selection(None)?.parameter_set;
    let sets = project.parameter_sets()?;
    if sets.is_empty() {
        println!("no parameter sets");
    }
    for s in sets {
        let mark = if Some(s.id) == active { "*" } else { " " };
        let values: Vec<String> = s.values.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
        println!("{mark} {:<20} {}", s.name, values.join(" "));
    }
    Ok(ExitCode::SUCCESS)
}

pub fn set_add(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    project.add_parameter_set(&ParameterSet {
        id: ParameterSetId::new(),
        name: name.trim().to_owned(),
        values: BTreeMap::new(),
    })?;
    Ok(ExitCode::SUCCESS)
}

pub fn set_rm(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let set = support::find_parameter_set(&project, name)?;
    project.delete_parameter_set(set.id)?;
    Ok(ExitCode::SUCCESS)
}

pub fn set_rename(dir: &Path, old: &str, new: &str) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let set = support::find_parameter_set(&project, old)?;
    project.rename_parameter_set(set.id, new.trim())?;
    Ok(ExitCode::SUCCESS)
}

pub fn param_list(dir: &Path) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let params = project.parameters()?;
    if params.is_empty() {
        println!("no parameters");
    }
    for p in params {
        println!("{p}");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn param_add(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    support::open_write(dir)?.add_parameter(name)?;
    Ok(ExitCode::SUCCESS)
}

pub fn param_rm(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    support::open_write(dir)?.delete_parameter(name)?;
    Ok(ExitCode::SUCCESS)
}

pub fn param_rename(dir: &Path, old: &str, new: &str) -> anyhow::Result<ExitCode> {
    support::open_write(dir)?.rename_parameter(old, new)?;
    Ok(ExitCode::SUCCESS)
}

/// `param set` (`Some`) and `param unset` (`None`).
pub fn param_value(
    dir: &Path,
    set: &str,
    name: &str,
    value: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let mut project = support::open_write(dir)?;
    let set = support::find_parameter_set(&project, set)?;
    project.set_parameter_value(set.id, name, value)?;
    Ok(ExitCode::SUCCESS)
}
