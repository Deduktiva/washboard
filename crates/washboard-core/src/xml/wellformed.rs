//! Well-formedness check for the editor (debounced on every edit) and the validation pipeline.
//!
//! libxml2 does the work, through the same wrapper and parser options as validation
//! (`validate::xsd`), so the editor's underline and the validation result can never disagree.
//! It covers the XML and Namespaces in XML rules: tag matching, one root element, names,
//! references, illegal characters, duplicate attributes, undeclared prefixes.

use crate::diag::{DiagSource, Diagnostic, pos_at_byte};

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
    let offset = byte_offset(text, line, column);
    let pos = (line > 0).then(|| pos_at_byte(text, offset));
    Some(WellFormednessError {
        offset,
        diagnostic: Diagnostic::error(
            DiagSource::WellFormedness,
            pos,
            format!("not well-formed: {message}"),
        ),
    })
}

/// Byte offset of a 1-based line and character column, clamped to the text.
fn byte_offset(text: &str, line: u32, column: u32) -> usize {
    let mut start = 0;
    for _ in 1..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let rest = &text[start..];
    let line_len = rest.find('\n').unwrap_or(rest.len());
    let chars = usize::try_from(column.saturating_sub(1)).unwrap_or(usize::MAX);
    start
        + rest[..line_len]
            .char_indices()
            .nth(chars)
            .map_or(line_len, |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_offsets_count_chars() {
        let t = "ab\nüx<\n";
        assert_eq!(byte_offset(t, 1, 1), 0);
        assert_eq!(byte_offset(t, 2, 2), 5); // 'x' after the two-byte 'ü'
        assert_eq!(byte_offset(t, 2, 99), 7); // clamped to the end of line 2
        assert_eq!(byte_offset(t, 9, 1), t.len());
    }
}
