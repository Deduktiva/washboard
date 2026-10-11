//! A project's WSDL with what is derived from it: the schema model (templates, completion) and
//! the compiled request schema (validation). Shared by the app, which loads it on a worker and
//! then reads it from the main thread and workers, and the CLI, which loads it per command.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::diag::Diagnostic;
use crate::schema::SchemaModel;
use crate::validate::RequestSchema;
use crate::wsdl::{self, SourceError, Sources, Wsdl};

/// The project's WSDL, and the schema model and request schema built from it on first use, so
/// a command that only lists operations compiles nothing. [`ProjectSchema::prepared`] builds
/// both up front, for callers that load on a worker and must not compile on the main thread.
#[derive(Debug)]
pub struct ProjectSchema {
    pub wsdl: Wsdl,
    model: OnceLock<Arc<SchemaModel>>,
    /// libxml2 schemas are `Send` but not `Sync`; validations of one project take turns.
    request: OnceLock<Result<Mutex<RequestSchema>, Vec<Diagnostic>>>,
}

impl ProjectSchema {
    /// Reads and analyzes the WSDL set in `wsdl_dir` (without `.previous`), starting at `entry`:
    /// [`Project::entry_wsdl`](super::Project::entry_wsdl) and
    /// [`Project::wsdl_dir`](super::Project::wsdl_dir), taken separately so a worker can load
    /// without the `Project`.
    pub fn load(entry: &Path, wsdl_dir: PathBuf) -> Result<ProjectSchema, SourceError> {
        let sources = Sources::from_disk(entry, &[wsdl_dir])?;
        Ok(ProjectSchema::new(wsdl::load(&sources)))
    }

    pub fn new(wsdl: Wsdl) -> ProjectSchema {
        ProjectSchema {
            wsdl,
            model: OnceLock::new(),
            request: OnceLock::new(),
        }
    }

    /// Builds the model and compiles the request schema now.
    pub fn prepared(self) -> ProjectSchema {
        self.compile_errors();
        self
    }

    /// Built on first use.
    pub fn model(&self) -> &Arc<SchemaModel> {
        self.model
            .get_or_init(|| Arc::new(SchemaModel::build(&self.wsdl.bundle)))
    }

    /// Why requests can't be schema-validated, if they can't. Compiles on first use.
    pub fn compile_errors(&self) -> &[Diagnostic] {
        match self.request() {
            Ok(_) => &[],
            Err(errors) => errors,
        }
    }

    /// `None` if the schemas did not compile ([`Self::compile_errors`] says why). Compiles on
    /// first use. A validation that panicked doesn't poison it for good: libxml2 keeps no
    /// state between validations.
    pub fn request_schema(&self) -> Option<MutexGuard<'_, RequestSchema>> {
        let mutex = self.request().as_ref().ok()?;
        Some(
            mutex
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn request(&self) -> &Result<Mutex<RequestSchema>, Vec<Diagnostic>> {
        self.request.get_or_init(|| {
            RequestSchema::compile(&self.wsdl.bundle)
                .map(|schema| Mutex::new(schema.with_model(self.model().clone())))
        })
    }
}
