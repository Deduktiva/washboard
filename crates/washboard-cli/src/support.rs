//! Shared plumbing: opening projects, finding things by name, loading the project's WSDL.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use washboard_core::diag;
use washboard_core::model::{OperationRef, QName, RequestMeta, Server};
use washboard_core::project::{Project, ProjectError, WsdlSet};
use washboard_core::schema::SchemaModel;
use washboard_core::validate::xsd::CompiledSchema;
use washboard_core::wsdl::{self, Binding, Operation, Protocol, Sources, Style, Wsdl};

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

pub fn find_request(project: &Project, name: &str) -> anyhow::Result<RequestMeta> {
    project
        .requests()?
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow!("no request named {name:?}"))
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

/// Reads and analyzes the project's current WSDL set (`wsdl/`, without `.previous`).
pub fn load_project_wsdl(project: &Project) -> anyhow::Result<Wsdl> {
    let entry = project.entry_wsdl()?;
    let sources = Sources::from_disk(&entry, &[project.wsdl_dir()])?;
    Ok(wsdl::load(&sources))
}

/// A WSDL set as supplied on the command line, checked like the New Project sheet does.
pub struct CheckedImport {
    pub wsdl: Wsdl,
    pub set: WsdlSet,
}

/// Runs the import check and the schema compile; prints every finding to stderr. Fails when
/// anything is unresolved or the schema set does not compile (PLAN §4 "Create project").
pub fn check_import(entry: &Path, extra: &[PathBuf]) -> anyhow::Result<CheckedImport> {
    let sources = Sources::from_disk(entry, extra)?;
    let w = wsdl::load(&sources);
    for d in &w.check.diagnostics {
        eprintln!("{d}");
    }
    if w.check.has_errors() {
        bail!("the import check found errors; nothing was changed");
    }
    match CompiledSchema::compile(&w.bundle) {
        Ok(schema) => {
            for d in schema.warnings() {
                eprintln!("{d}");
            }
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("{d}");
            }
            if diag::has_errors(&diags) {
                bail!("the schema set does not compile; nothing was changed");
            }
        }
    }
    let set = WsdlSet::from_import(&sources, &w);
    Ok(CheckedImport { wsdl: w, set })
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

/// Builds the schema model and the request envelope for an operation.
pub fn envelope(w: &Wsdl, op: &OperationRef) -> anyhow::Result<String> {
    let model = SchemaModel::build(&w.bundle);
    Ok(washboard_core::soap::request_envelope(
        w,
        &model,
        op,
        &Default::default(),
    )?)
}

/// `2026-10-06T14:03:12.123456789Z` → `2026-10-06 14:03:12Z`, for human output.
pub fn short_time(full: &str) -> String {
    match (full.get(..10), full.get(11..19)) {
        (Some(d), Some(t)) => format!("{d} {t}Z"),
        _ => full.to_owned(),
    }
}
