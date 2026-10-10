//! The import sheet shared by New Project and Replace WSDL (PLAN §4 "Create project",
//! "Replace WSDL"): the import check and schema compile run on a worker whenever the chosen
//! files change; Create (or Replace) is enabled only when nothing is unresolved and the schema
//! set compiles.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use washboard_core::diag::{self, Diagnostic};
use washboard_core::model::{OperationRef, RequestId};
use washboard_core::project::{OpenProject, Project, WsdlSet};
use washboard_core::validate::validate_request;
use washboard_core::validate::xsd::CompiledSchema;
use washboard_core::wsdl::{self, ImportCheck, StructuralReport};

use crate::app::{App, ModelError, ProjectKey};
use crate::event::Event;
use crate::window::ProjectSchema;

/// Which sheet: there is at most one New Project sheet and one Replace WSDL sheet per project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportTarget {
    NewProject,
    ReplaceWsdl(ProjectKey),
}

/// The sheet's state.
#[derive(Debug, Default)]
pub struct ImportSheet {
    /// New Project only.
    pub name: String,
    /// New Project only: the project folder is created inside it, named after the project.
    pub parent: Option<PathBuf>,
    pub entry: Option<PathBuf>,
    /// Additional XSD/WSDL files or folders.
    pub extra: Vec<PathBuf>,
    pub check: CheckState,
    generation: u64,
}

#[derive(Debug, Default)]
pub enum CheckState {
    /// No WSDL chosen yet.
    #[default]
    Empty,
    Checking,
    Done(Box<CheckedImport>),
    /// The files could not be read at all.
    Failed(String),
}

/// What the import check found.
#[derive(Debug)]
pub struct CheckedImport {
    /// Every reference with its resolution, and the findings.
    pub check: ImportCheck,
    pub report: StructuralReport,
    /// Schema compile errors and warnings; empty if the check already failed.
    pub compile: Vec<Diagnostic>,
    /// `(port, address)` of each SOAP 1.1 port, offered as servers after creation.
    pub addresses: Vec<(String, String)>,
    /// Files read because a relative reference named them and they exist next to the
    /// importing file; the user did not add them.
    pub found: Vec<PathBuf>,
    set: WsdlSet,
    operations: Vec<OperationRef>,
}

impl CheckedImport {
    /// Nothing unresolved and the schema set compiles.
    pub fn is_usable(&self) -> bool {
        !self.check.has_errors() && !diag::has_errors(&self.compile)
    }
}

impl ImportSheet {
    /// Whether Create / Replace is enabled.
    pub fn can_finish(&self, target: ImportTarget) -> bool {
        let named = match target {
            ImportTarget::NewProject => !self.name.trim().is_empty() && self.parent.is_some(),
            ImportTarget::ReplaceWsdl(_) => true,
        };
        named && matches!(&self.check, CheckState::Done(c) if c.is_usable())
    }
}

/// What replacing the WSDL changed, for the report (WP-REPLACE-REPORT shows it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaceOutcome {
    pub added: Vec<OperationRef>,
    pub removed: Vec<OperationRef>,
    /// Requests that fail validation against the new WSDL.
    pub invalid: Vec<RequestId>,
}

/// A server suggested from a `soap:address`, added only once the user confirms its URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedServer {
    pub port: String,
    pub url: String,
}

impl App {
    /// Opens the sheet (or keeps the open one).
    pub fn begin_import(&mut self, target: ImportTarget) {
        self.imports.entry(target).or_default();
        self.events.push(Event::ImportChanged { target });
    }

    pub fn import_sheet(&self, target: ImportTarget) -> Option<&ImportSheet> {
        self.imports.get(&target)
    }

    pub fn cancel_import(&mut self, target: ImportTarget) {
        if self.imports.remove(&target).is_some() {
            self.events.push(Event::ImportChanged { target });
        }
    }

    /// New Project: the name and the folder it goes in.
    pub fn set_import_destination(
        &mut self,
        name: &str,
        parent: Option<PathBuf>,
    ) -> Result<(), ModelError> {
        let target = ImportTarget::NewProject;
        let sheet = self.imports.get_mut(&target).ok_or(ModelError::NoImport)?;
        sheet.name = name.to_owned();
        sheet.parent = parent;
        self.events.push(Event::ImportChanged { target });
        Ok(())
    }

    /// The WSDL and supporting files or folders; starts the check.
    pub fn set_import_files(
        &mut self,
        target: ImportTarget,
        entry: PathBuf,
        extra: Vec<PathBuf>,
    ) -> Result<(), ModelError> {
        let generation = self.next();
        let sheet = self.imports.get_mut(&target).ok_or(ModelError::NoImport)?;
        sheet.entry = Some(entry.clone());
        sheet.extra = extra.clone();
        sheet.check = CheckState::Checking;
        sheet.generation = generation;
        self.events.push(Event::ImportChanged { target });
        self.spawn(
            move || check_import(&entry, &extra),
            move |app, result| app.import_checked(target, generation, result),
        );
        Ok(())
    }

    fn import_checked(
        &mut self,
        target: ImportTarget,
        generation: u64,
        result: Result<CheckedImport, String>,
    ) {
        let Some(sheet) = self.imports.get_mut(&target) else {
            return;
        };
        if sheet.generation != generation {
            return;
        }
        sheet.check = match result {
            Ok(checked) => CheckState::Done(Box::new(checked)),
            Err(message) => CheckState::Failed(message),
        };
        self.events.push(Event::ImportChanged { target });
    }

    /// Create (New Project): copies the files, creates and opens the project, and offers the
    /// WSDL's addresses as servers.
    pub fn create_project(&mut self) -> Result<ProjectKey, ModelError> {
        let target = ImportTarget::NewProject;
        let sheet = self.imports.get(&target).ok_or(ModelError::NoImport)?;
        if !sheet.can_finish(target) {
            return Err(ModelError::ImportNotReady);
        }
        let CheckState::Done(checked) = &sheet.check else {
            return Err(ModelError::ImportNotReady);
        };
        let parent = sheet.parent.as_ref().ok_or(ModelError::ImportNotReady)?;
        let name = sheet.name.trim();
        let folder = parent.join(name);
        let project = Project::create(&folder, name, &checked.set)?;
        let suggested = checked
            .addresses
            .iter()
            .map(|(port, url)| SuggestedServer {
                port: port.clone(),
                url: url.clone(),
            })
            .collect();
        self.imports.remove(&target);
        self.events.push(Event::ImportChanged { target });
        let key = self.add_project(project, OpenProject::new(&folder));
        if let Some(window) = self.window_mut(key) {
            window.suggested_servers = suggested;
        }
        self.note_recent(&folder);
        self.update_welcome();
        Ok(key)
    }

    /// Settings ▸ Servers: adds a suggested server once the user has confirmed (or edited) its
    /// URL. Nothing connects before that.
    pub fn confirm_suggested_server(
        &mut self,
        key: ProjectKey,
        index: usize,
        url: &str,
    ) -> Result<(), ModelError> {
        let window = self.window(key)?;
        if index >= window.suggested_servers.len() {
            return Ok(());
        }
        let suggestion = window.suggested_servers.remove(index);
        let id = self.add_server(key)?;
        let window = self.window(key)?;
        let mut server = window
            .servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or(ModelError::NoServer)?;
        server.name = suggestion.port;
        server.url = url.to_owned();
        self.update_server(key, &server, None)
    }

    /// Replace (Replace WSDL): swaps the project's WSDL set, then reloads the schema and
    /// re-validates every request on a worker. [`Event::WsdlReplaced`] follows with the
    /// outcome. Requests are never rewritten.
    pub fn replace_wsdl(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let target = ImportTarget::ReplaceWsdl(key);
        let sheet = self.imports.get(&target).ok_or(ModelError::NoImport)?;
        if !sheet.can_finish(target) {
            return Err(ModelError::ImportNotReady);
        }
        let CheckState::Done(checked) = &sheet.check else {
            return Err(ModelError::ImportNotReady);
        };
        let set = checked.set.clone();
        let new_operations = checked.operations.clone();
        self.flush(key)?;
        let window = self.window(key)?;
        let old_operations = match &window.schema {
            crate::window::SchemaState::Ready(s) => s.wsdl.supported_operations(),
            _ => Vec::new(),
        };
        window.project.replace_wsdl(&set)?;
        let entry = window.project.entry_wsdl()?;
        let wsdl_dir = window.project.wsdl_dir();
        let mut requests = Vec::new();
        for meta in window.project.requests()? {
            // A request that can't be read now can't be checked; it is left out.
            if let Ok(text) = window.project.read_request(meta.id) {
                requests.push((meta.id, meta.operation, text));
            }
        }
        window.schema = crate::window::SchemaState::Loading;
        self.imports.remove(&target);
        self.events.push(Event::ImportChanged { target });
        self.spawn(
            move || {
                let schema = ProjectSchema::load(&entry, wsdl_dir)?;
                let invalid = invalid_requests(&schema, &requests);
                Ok((schema, invalid))
            },
            move |app, loaded: Result<(ProjectSchema, Vec<RequestId>), String>| {
                let outcome = loaded.as_ref().ok().map(|(_, invalid)| ReplaceOutcome {
                    added: missing_from(&new_operations, &old_operations),
                    removed: missing_from(&old_operations, &new_operations),
                    invalid: invalid.clone(),
                });
                app.schema_loaded(key, loaded.map(|(schema, _)| schema));
                let Some(window) = app.window_mut(key) else {
                    return;
                };
                if let Some(outcome) = &outcome {
                    for row in &mut window.sidebar.requests {
                        row.invalid = outcome.invalid.contains(&row.id);
                    }
                }
                window.replace_outcome = outcome;
                app.events.push(Event::SidebarChanged { project: key });
                app.events.push(Event::WsdlReplaced { project: key });
            },
        );
        Ok(())
    }
}

/// Runs on a worker.
fn check_import(entry: &Path, extra: &[PathBuf]) -> Result<CheckedImport, String> {
    let wsdl::Loaded {
        sources,
        wsdl: w,
        found,
    } = wsdl::load_from_disk(entry, extra).map_err(|e| e.to_string())?;
    let compile = if w.check.has_errors() {
        Vec::new()
    } else {
        match CompiledSchema::compile(&w.bundle) {
            Ok(schema) => schema.warnings().to_vec(),
            Err(diagnostics) => diagnostics,
        }
    };
    let set = WsdlSet::from_import(&sources, &w);
    Ok(CheckedImport {
        addresses: w.soap11_addresses(),
        operations: w.supported_operations(),
        check: w.check,
        report: w.report,
        compile,
        found,
        set,
    })
}

/// The operations of `a` that `b` lacks, in `a`'s order.
fn missing_from(a: &[OperationRef], b: &[OperationRef]) -> Vec<OperationRef> {
    a.iter().filter(|op| !b.contains(op)).cloned().collect()
}

/// Validates every request against the new schema; without a usable one, every request
/// counts as invalid only if it isn't well-formed.
fn invalid_requests(
    schema: &ProjectSchema,
    requests: &[(RequestId, Option<OperationRef>, String)],
) -> Vec<RequestId> {
    let request_schema = schema.request_schema();
    requests
        .iter()
        .filter(|(_, hint, text)| match &request_schema {
            Some(rs) => validate_request(&schema.wsdl, rs, text, hint.as_ref()).has_errors(),
            None => washboard_core::xml::check_well_formed(text).is_err(),
        })
        .map(|(id, _, _)| *id)
        .collect()
}

/// Sheets keyed by target; lives in [`App`].
pub(crate) type Imports = HashMap<ImportTarget, ImportSheet>;
