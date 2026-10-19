//! Shared plumbing: opening projects, finding things by name, loading the project's WSDL.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use washboard_core::interpolate::Values;
use washboard_core::model::{
    OperationRef, ParameterSet, ParameterSetId, QName, RequestMeta, Server,
};
use washboard_core::project::{self, CheckedImport, Project, ProjectError, ProjectSchema};
use washboard_core::wsdl::{Binding, Operation, Protocol, Style, Wsdl};

/// Opens without the lock, for commands that only read: they work while the app (or a writing
/// command) has the project open.
pub fn open_read(dir: &Path) -> anyhow::Result<Project> {
    Project::open_read_only(dir).with_context(|| format!("cannot open project {}", dir.display()))
}

/// Opens with the folder lock, for commands that write.
pub fn open_write(dir: &Path) -> anyhow::Result<Project> {
    match Project::open(dir) {
        Ok(p) => Ok(p),
        Err(e @ ProjectError::AlreadyOpen(_)) => Err(anyhow!(
            "{e} (or another washboard command); close it there and try again"
        )),
        Err(e) => Err(e).with_context(|| format!("cannot open project {}", dir.display())),
    }
}

/// A request's path relative to `requests/`: `GetCustomer/GetCustomer 1`, or just the name
/// at the top level. What `request list` prints and every `<name>` argument accepts.
pub fn request_path(r: &RequestMeta) -> String {
    if r.folder.is_empty() {
        r.name.clone()
    } else {
        format!("{}/{}", r.folder, r.name)
    }
}

/// Finds a request by name, or by path when the argument has a `/`. Names are unique, but two
/// files of the same name can arrive in different folders from Finder or git; a bare name
/// then fails and lists their paths.
pub fn find_request(project: &Project, name: &str) -> anyhow::Result<RequestMeta> {
    let by_path = name.contains('/');
    let mut matches: Vec<RequestMeta> = project
        .requests()?
        .into_iter()
        .filter(|r| {
            if by_path {
                request_path(r) == name.trim_start_matches('/')
            } else {
                r.name == name
            }
        })
        .collect();
    match matches.len() {
        0 => bail!("no request named {name:?}"),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "several requests are named {name:?}; give the path: {}",
            matches
                .iter()
                .map(request_path)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn find_server(project: &Project, name: &str) -> anyhow::Result<Server> {
    let mut matches: Vec<Server> = project
        .servers()?
        .into_iter()
        .filter(|s| s.name == name)
        .collect();
    match matches.len() {
        0 => bail!("no server named {name:?}"),
        1 => Ok(matches.remove(0)),
        n => bail!("{n} servers are named {name:?}; rename one of them in the app"),
    }
}

/// Sets are matched by name ignoring case and surrounding space, as the project compares them.
pub fn find_parameter_set(project: &Project, name: &str) -> anyhow::Result<ParameterSet> {
    let folded = name.trim().to_lowercase();
    let mut matches: Vec<ParameterSet> = project
        .parameter_sets()?
        .into_iter()
        .filter(|s| s.name.trim().to_lowercase() == folded)
        .collect();
    match matches.len() {
        0 => bail!("no parameter set named {name:?}"),
        1 => Ok(matches.remove(0)),
        n => bail!("{n} parameter sets are named {name:?}; rename one of them in the app"),
    }
}

/// What `request`'s placeholders resolve against: `--set`, else the request's pin, else the
/// project's active set (`docs/specs/parameters.md`, "Selection").
pub fn parameter_values(
    project: &Project,
    request: &RequestMeta,
    set: Option<&str>,
) -> anyhow::Result<(Option<ParameterSetId>, Values)> {
    let id = match set {
        Some(name) => Some(find_parameter_set(project, name)?.id),
        None => project.selection(Some(request))?.parameter_set,
    };
    Ok((id, project.parameter_values(id)?))
}

/// Reads and analyzes the project's current WSDL set (`wsdl/`, without `.previous`); the schema
/// model and the request schema are built when a command first needs them.
pub fn load_project_schema(project: &Project) -> anyhow::Result<ProjectSchema> {
    Ok(ProjectSchema::load(
        &project.entry_wsdl()?,
        project.wsdl_dir(),
    )?)
}

/// Runs the import check and the schema compile, as the New Project sheet does; prints every
/// finding to stderr. Fails when anything is unresolved or the schema set does not compile
/// (`docs/specs/new-project.md`).
pub fn check_import(entry: &Path, extra: &[PathBuf]) -> anyhow::Result<CheckedImport> {
    let checked = project::check_import(entry, extra)?;
    for path in &checked.found {
        eprintln!(
            "found {} next to the file that references it",
            path.display()
        );
    }
    for d in checked
        .wsdl
        .check
        .diagnostics
        .iter()
        .chain(&checked.compile)
    {
        eprintln!("{d}");
    }
    if checked.wsdl.check.has_errors() {
        bail!("the import check found errors; nothing was changed");
    }
    if !checked.is_usable() {
        bail!("the schema set does not compile; nothing was changed");
    }
    Ok(checked)
}

pub fn protocol_label(p: &Protocol) -> &'static str {
    match p {
        Protocol::Soap11 => "SOAP 1.1",
        Protocol::Soap12 => "SOAP 1.2",
        Protocol::Other { .. } => "not SOAP",
    }
}

pub fn style_label(s: Style) -> &'static str {
    match s {
        Style::Document => "document",
        Style::Rpc => "rpc",
    }
}

/// Resolves `Operation`, `Binding#Operation` or `{ns}Binding#Operation` to one supported
/// operation. Unsupported matches are reported with their reason, never silently skipped.
pub fn resolve_operation(w: &Wsdl, spec: &str) -> anyhow::Result<OperationRef> {
    let (binding, name) = match spec.rsplit_once('#') {
        Some((b, o)) => (
            Some(
                b.parse::<QName>()
                    .map_err(|e| anyhow!("bad binding in {spec:?}: {e}"))?,
            ),
            o,
        ),
        None => (None, spec),
    };
    let matches: Vec<(&Binding, &Operation)> = w
        .definitions
        .bindings
        .iter()
        .filter(|b| match &binding {
            None => true,
            Some(q) if q.ns.is_empty() => b.name.local == q.local,
            Some(q) => &b.name == q,
        })
        .flat_map(|b| b.operations.iter().map(move |o| (b, o)))
        .filter(|(_, o)| o.name == name)
        .collect();
    let supported: Vec<OperationRef> = matches
        .iter()
        .filter(|(_, o)| o.is_supported())
        .map(|(b, o)| b.operation_ref(o))
        .collect();
    if let [one] = supported.as_slice() {
        return Ok(one.clone());
    }
    if !supported.is_empty() {
        bail!(
            "operation {spec:?} exists in several bindings; qualify it: {}",
            supported
                .iter()
                .map(|o| format!("{}#{}", o.binding.local, o.operation))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    match matches.first() {
        None => bail!("no operation {spec:?} in the WSDL"),
        Some((b, o)) => {
            let reason = o
                .support
                .reason()
                .map(ToString::to_string)
                .unwrap_or_default();
            bail!(
                "operation {}#{} is not supported: {reason}",
                b.name.local,
                o.name
            )
        }
    }
}

/// `2026-10-06T14:03:12.123456789Z` → `2026-10-06 14:03:12Z`, for human output.
pub fn short_time(full: &str) -> String {
    match (full.get(..10), full.get(11..19)) {
        (Some(d), Some(t)) => format!("{d} {t}Z"),
        _ => full.to_owned(),
    }
}
