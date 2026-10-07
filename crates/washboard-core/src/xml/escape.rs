//! Escaping text for XML output. The one place that does it, so generated schemas, templates
//! and edits to user documents all escape the same way.

use std::borrow::Cow;

/// Escapes character data (element content): `&`, `<`, `>`.
pub fn escape_text(text: &str) -> Cow<'_, str> {
    quick_xml::escape::partial_escape(text)
}

/// Escapes a value for a single- or double-quoted attribute.
///
/// Beyond `&`, `<` and both quotes this also writes tab, line feed and carriage return as
/// character references: a parser normalizes literal whitespace in attribute values to spaces,
/// so a `schemaLocation` or namespace URI containing them would otherwise change meaning.
/// (`quick_xml::escape::escape` leaves them literal, hence not used here.)
pub fn escape_attr(value: &str) -> Cow<'_, str> {
    if !value.contains(['&', '<', '"', '\'', '\n', '\r', '\t']) {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_values() {
        assert_eq!(escape_attr("a&b<\"'"), "a&amp;b&lt;&quot;&apos;");
        assert_eq!(escape_attr("x\ty\r\nz>"), "x&#9;y&#13;&#10;z>");
        assert!(matches!(escape_attr("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn text_content() {
        assert_eq!(escape_text("a<b & c>d \"'"), "a&lt;b &amp; c&gt;d \"'");
    }
}
