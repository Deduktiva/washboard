//! The import sheet shared by New Project and Replace WSDL (`docs/specs/new-project.md`): the
//! import check and schema compile run on a worker whenever the chosen files change; Create (or
//! Replace) is enabled only when nothing is unresolved and the schema set compiles.

use std::collections::HashMap;
use std::path::PathBuf;

pub use washboard_core::project::CheckedImport;
use washboard_core::project::{OpenProject, Project, ReplaceReport, check_import, request_texts};

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

impl ImportSheet {
    /// Whether Create / Replace is enabled.
    pub fn can_finish(&self, target: ImportTarget) -> bool {
        self.finished(target).is_some()
    }

    /// The check result when Create / Replace is enabled.
    fn finished(&self, target: ImportTarget) -> Option<&CheckedImport> {
        let named = match target {
            ImportTarget::NewProject => !self.name.trim().is_empty() && self.parent.is_some(),
            ImportTarget::ReplaceWsdl(_) => true,
        };
        match &self.check {
            CheckState::Done(c) if named && c.is_usable() => Some(c),
            _ => None,
        }
    }
}

/// What replacing the WSDL changed, for the report; computed by the core so the CLI's
/// `project replace-wsdl` says the same.
pub type ReplaceOutcome = ReplaceReport;

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
            move || check_import(&entry, &extra).map_err(|e| e.to_string()),
            move |app, result| app.import_checked(target, generation, result),
        );
        Ok(())
    }

    /// The sheet and its check result, when Create / Replace may go ahead.
    fn finished_import(
        &self,
        target: ImportTarget,
    ) -> Result<(&ImportSheet, &CheckedImport), ModelError> {
        let sheet = self.imports.get(&target).ok_or(ModelError::NoImport)?;
        let checked = sheet.finished(target).ok_or(ModelError::ImportNotReady)?;
        Ok((sheet, checked))
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

    /// Create (New Project): copies the files, creates and opens the project. The WSDL's
    /// addresses become servers only through Add as Server… ([`ProjectWindow::wsdl_servers`]).
    ///
    /// [`ProjectWindow::wsdl_servers`]: crate::ProjectWindow::wsdl_servers
    pub fn create_project(&mut self) -> Result<ProjectKey, ModelError> {
        let target = ImportTarget::NewProject;
        let (sheet, checked) = self.finished_import(target)?;
        let parent = sheet.parent.as_ref().ok_or(ModelError::ImportNotReady)?;
        let name = sheet.name.trim();
        let folder = parent.join(name);
        let project = Project::create(&folder, name, &checked.set)?;
        self.imports.remove(&target);
        self.events.push(Event::ImportChanged { target });
        let key = self.add_project(project, OpenProject::new(&folder));
        self.note_recent(&folder);
        self.update_welcome();
        Ok(key)
    }

    /// Replace (Replace WSDL): swaps the project's WSDL set, then reloads the schema and
    /// re-validates every request on a worker. [`Event::WsdlReplaced`] follows with the
    /// outcome. Requests are never rewritten.
    pub fn replace_wsdl(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let target = ImportTarget::ReplaceWsdl(key);
        let (_, checked) = self.finished_import(target)?;
        let set = checked.set.clone();
        self.flush(key)?;
        let window = self.window(key)?;
        let old_operations = window
            .schema
            .ready()
            .map(|s| s.wsdl.supported_operations())
            .unwrap_or_default();
        window.project.replace_wsdl(&set)?;
        let entry = window.project.entry_wsdl()?;
        let wsdl_dir = window.project.wsdl_dir();
        let requests = request_texts(&window.project)?;
        window.schema = crate::window::SchemaState::Loading;
        self.imports.remove(&target);
        self.events.push(Event::ImportChanged { target });
        // The old WSDL's addresses go until the new one has loaded.
        self.events.push(Event::ServersChanged { project: key });
        self.spawn(
            move || {
                let schema = ProjectSchema::load(&entry, wsdl_dir)?;
                let report = ReplaceReport::new(
                    &old_operations,
                    &schema.wsdl,
                    schema.request_schema().as_deref(),
                    &requests,
                );
                Ok((schema, report))
            },
            move |app, loaded: Result<(ProjectSchema, ReplaceOutcome), String>| {
                let (loaded, outcome) = match loaded {
                    Ok((schema, report)) => (Ok(schema), Some(report)),
                    Err(e) => (Err(e), None),
                };
                app.schema_loaded(key, loaded);
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

/// Sheets keyed by target; lives in [`App`].
pub(crate) type Imports = HashMap<ImportTarget, ImportSheet>;
