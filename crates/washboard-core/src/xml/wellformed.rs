//! Well-formedness check for the editor (debounced on every edit) and the validation pipeline.
//!
//! libxml2 does the work, through the same wrapper and parser options as validation
//! (`validate::xsd`), so the editor's underline and the validation result can never disagree.
//! It covers the XML and Namespaces in XML rules: tag matching, one root element, names,
//! references, illegal characters, duplicate attributes, undeclared prefixes.

use crate::diag::{DiagSource, Diagnostic, LineIndex, TextPos};

/// The first well-formedness error of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WellFormednessError {
    /// Byte offset the error points at, for the editor's underline. [`Self::diagnostic`] carries
    /// the same position as line/column.
    pub offset: usize,
    pub diagnostic: Diagnostic,
}

/// Checks `text` (already decoded, see [`super::decode`]) and returns the first error.
///
/// Messages start with "not well-formed:". Positions are where libxml2 detected the problem,
/// which is usually just past the offending construct (e.g. after a mismatched end tag).
pub fn check_well_formed(text: &str) -> Result<(), Diagnostic> {
    match well_formedness_error(text) {
        Some(e) => Err(e.diagnostic),
        None => Ok(()),
    }
}

/// Like [`check_well_formed`], with the byte offset of the error as well.
pub fn well_formedness_error(text: &str) -> Option<WellFormednessError> {
    let (line, column, message) = crate::validate::xsd::first_well_formedness_error(text)?;
    let lines = LineIndex::new(text);
    let offset = lines.byte_clamped(TextPos { line, column });
    let pos = (line > 0).then(|| lines.pos(offset));
    Some(WellFormednessError {
        offset,
        diagnostic: Diagnostic::error(
            DiagSource::WellFormedness,
            pos,
            format!("not well-formed: {message}"),
        ),
    })
}
