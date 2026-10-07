//! Diagnostics shown in the editor's issues bar and gutter.

use std::fmt;

/// A position in a text document as the editor shows it.
///
/// Both fields are 1-based. `column` counts Unicode scalar values (chars), not bytes,
/// and a leading BOM is not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
    pub line: u32,
    pub column: u32,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub source: DiagSource,
    /// `None` when the problem has no meaningful location (e.g. "no operation matches").
    pub pos: Option<TextPos>,
    pub message: String,
}

impl Diagnostic {
    pub fn error(source: DiagSource, pos: Option<TextPos>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            source,
            pos,
            message: message.into(),
        }
    }

    pub fn warning(source: DiagSource, pos: Option<TextPos>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            source,
            pos,
            message: message.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
