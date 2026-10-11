//! The import check behind New Project and Replace WSDL (`docs/specs/new-project.md`), shared by
//! the app's import sheet and the CLI's `new-project` and `replace-wsdl`, so both accept and
//! refuse the same file sets.

use std::path::{Path, PathBuf};

use crate::diag::{self, Diagnostic};
use crate::validate::xsd::CompiledSchema;
use crate::wsdl::{self, SourceError, Wsdl};

use super::WsdlSet;

/// What [`check_import`] found. Plain values, so it can be built on a worker and handed over.
#[derive(Debug, Clone)]
pub struct CheckedImport {
    /// The analyzed set: [`Wsdl::check`] has every reference with its resolution and the
    /// findings, [`Wsdl::report`] the structural summary.
    pub wsdl: Wsdl,
    /// Schema compile errors and warnings; empty when the import check already failed, since
    /// compiling an incomplete set only repeats its errors.
    pub compile: Vec<Diagnostic>,
    /// Files read because a relative reference named them and they exist next to the
    /// importing file; the user did not add them.
    pub found: Vec<PathBuf>,
    /// The files to copy into the project.
    pub set: WsdlSet,
}

impl CheckedImport {
    /// Nothing unresolved and the schema set compiles: Create / Replace may go ahead.
    pub fn is_usable(&self) -> bool {
        !self.wsdl.check.has_errors() && !diag::has_errors(&self.compile)
    }
}

/// Reads `entry` and `extra` (files or folders) with [`wsdl::load_from_disk`], runs the import
/// check and compiles the schema set. Unresolved references and compile errors are findings in
/// the result ([`CheckedImport::is_usable`]); `Err` only when the files cannot be read at all.
pub fn check_import(entry: &Path, extra: &[PathBuf]) -> Result<CheckedImport, SourceError> {
    let wsdl::Loaded {
        sources,
        wsdl,
        found,
    } = wsdl::load_from_disk(entry, extra)?;
    let compile = if wsdl.check.has_errors() {
        Vec::new()
    } else {
        match CompiledSchema::compile(&wsdl.bundle) {
            Ok(schema) => schema.warnings().to_vec(),
            Err(diagnostics) => diagnostics,
        }
    };
    let set = WsdlSet::from_import(&sources, &wsdl);
    Ok(CheckedImport {
        wsdl,
        compile,
        found,
        set,
    })
}
