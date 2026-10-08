//! Diagnostics shown in the editor's issues bar and gutter.

use std::fmt;

use crate::model::QName;

/// A position in a text document as the editor shows it.
///
/// Both fields are 1-based. `column` counts Unicode scalar values (chars), not bytes,
/// and a leading BOM is not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
    pub line: u32,
    pub column: u32,
}

impl From<roxmltree::TextPos> for TextPos {
    /// roxmltree counts the same way: 1-based lines and char columns.
    fn from(p: roxmltree::TextPos) -> Self {
        TextPos {
            line: p.row,
            column: p.col,
        }
    }
}

/// A range in a text document, in [`TextPos`] coordinates. `end` is exclusive: the span
/// `1:5..1:8` covers columns 5, 6 and 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextSpan {
    pub start: TextPos,
    pub end: TextPos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Error,
}

/// Which stage produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagSource {
    /// The document is not well-formed XML.
    WellFormedness,
    /// SOAP envelope structure or operation dispatch.
    Soap,
    /// XSD validation of headers or body content.
    Schema,
    /// WSDL/XSD import resolution (project creation, WSDL replacement).
    Import,
}

/// What libxml2 said about a schema error, as data rather than message text (PLAN §5.2
/// "Validation errors"), so later stages can act on the kind of error and the node it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagDetail {
    /// libxml2's `xmlParserErrors` code, e.g. 1876 (`XML_SCHEMAV_CVC_TYPE_2`, abstract type).
    pub code: i32,
    /// Document-order index, from 0, of the element the error refers to among all elements of
    /// the validated document. Content of entity references is not counted.
    pub element_index: Option<usize>,
    /// For attribute errors, the attribute (namespace URI, local name).
    pub attribute: Option<QName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub source: DiagSource,
    /// `None` when the problem has no meaningful location (e.g. "no operation matches").
    pub pos: Option<TextPos>,
    pub message: String,
    /// The exact text the problem is about, for underlining: an attribute, an element's text,
    /// a start tag. `None` means only `pos` is known. It need not start at `pos`: schema errors
    /// keep `pos` at the element's `<` while an attribute error's span covers the attribute.
    pub span: Option<TextSpan>,
    /// Set on libxml2 schema validation errors.
    /// Boxed: most diagnostics have none, and `Result<_, Diagnostic>` should stay small.
    pub detail: Option<Box<DiagDetail>>,
}

impl Diagnostic {
    pub fn error(source: DiagSource, pos: Option<TextPos>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            source,
            pos,
            message: message.into(),
            span: None,
            detail: None,
        }
    }

    pub fn warning(source: DiagSource, pos: Option<TextPos>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            source,
            pos,
            message: message.into(),
            span: None,
            detail: None,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match self.pos {
            Some(p) => write!(f, "{}:{}: {sev}: {}", p.line, p.column, self.message),
            None => write!(f, "{sev}: {}", self.message),
        }
    }
}

impl Diagnostic {
    pub fn with_span(mut self, span: TextSpan) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_detail(mut self, detail: DiagDetail) -> Self {
        self.detail = Some(Box::new(detail));
        self
    }

    /// Errors block (sending, project creation); warnings never do.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Whether any diagnostic is an error, i.e. whether the thing checked is blocked.
pub fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(Diagnostic::is_error)
}

/// Number of errors, for summaries like "3 errors". Warnings are not counted.
pub fn error_count(diags: &[Diagnostic]) -> usize {
    diags.iter().filter(|d| d.is_error()).count()
}

/// Converts a byte offset into `text` to a [`TextPos`].
///
/// Offsets past the end clamp to the end. Offsets inside a multi-byte char point at that char.
pub fn pos_at_byte(text: &str, byte: usize) -> TextPos {
    let byte = byte.min(text.len());
    let mut line = 1u32;
    let mut column = 1u32;
    for (i, ch) in text.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    TextPos { line, column }
}

/// Precomputed line starts, so many positions in one large text stay cheap.
#[derive(Debug)]
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    /// Same semantics as [`pos_at_byte`].
    pub fn pos(&self, byte: usize) -> TextPos {
        let byte = byte.min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= byte).max(1);
        let start = self.starts[line - 1];
        let column = self.text.get(start..byte).map_or(0, |s| s.chars().count());
        TextPos {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(column + 1).unwrap_or(u32::MAX),
        }
    }

    /// The span covering the byte range `bytes`.
    pub fn span(&self, bytes: std::ops::Range<usize>) -> TextSpan {
        TextSpan {
            start: self.pos(bytes.start),
            end: self.pos(bytes.end),
        }
    }

    /// The inverse of [`Self::pos`]: the byte offset of `pos`, or `None` if it is not in the
    /// text. A column one past the end of a line is that line's end.
    pub fn byte(&self, pos: TextPos) -> Option<usize> {
        let line = usize::try_from(pos.line).ok()?.checked_sub(1)?;
        let column = usize::try_from(pos.column).ok()?.checked_sub(1)?;
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |&s| s - 1);
        let content = self.text.get(start..end)?;
        let offset = content
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(content.len()))
            .nth(column)?;
        Some(start + offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_matches_pos_at_byte() {
        let t = "ab\nüx<\n\nz";
        let li = LineIndex::new(t);
        for b in 0..=t.len() + 2 {
            if t.is_char_boundary(b.min(t.len())) {
                assert_eq!(li.pos(b), pos_at_byte(t, b), "byte {b}");
            }
        }
    }

    #[test]
    fn byte_inverts_pos() {
        let t = "ab\nüx\n";
        let li = LineIndex::new(t);
        for b in [0, 1, 2, 3, 5, 6, 7] {
            assert_eq!(li.byte(li.pos(b)), Some(b), "byte {b}");
        }
        assert_eq!(li.byte(TextPos { line: 2, column: 4 }), None);
        assert_eq!(li.byte(TextPos { line: 4, column: 1 }), None);
        assert_eq!(li.byte(TextPos { line: 0, column: 1 }), None);
    }

    #[test]
    fn pos_counts_chars_not_bytes() {
        let t = "ab\nüx<";
        assert_eq!(pos_at_byte(t, 0), TextPos { line: 1, column: 1 });
        assert_eq!(pos_at_byte(t, 3), TextPos { line: 2, column: 1 });
        // 'x' is after the two-byte 'ü'
        assert_eq!(pos_at_byte(t, 5), TextPos { line: 2, column: 2 });
        assert_eq!(pos_at_byte(t, 100), TextPos { line: 2, column: 4 });
    }
}
