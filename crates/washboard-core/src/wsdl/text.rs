//! Small text helpers: byte offset → [`TextPos`], start-tag lexing, splicing edits.
//!
//! Bundle documents are produced by splicing the decoded source text rather than
//! re-serializing a tree, so everything except the rewritten attributes stays byte-identical
//! and libxml2's line numbers still point into the user's files.

use std::ops::Range;

use crate::diag::TextPos;

/// Precomputed line starts, so many positions in one large file stay cheap.
#[derive(Debug)]
pub(crate) struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    /// Same semantics as [`crate::diag::pos_at_byte`].
    pub(crate) fn pos(&self, byte: usize) -> TextPos {
        let byte = byte.min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= byte).max(1);
        let start = self.starts[line - 1];
        let column = self.text.get(start..byte).map_or(0, |s| s.chars().count());
        TextPos {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(column + 1).unwrap_or(u32::MAX),
        }
    }
}

/// Lexical view of a start tag, used where roxmltree does not expose ranges
/// (namespace declarations, the end of the element name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartTag {
    /// Byte offset just after the element name.
    pub name_end: usize,
    /// Attribute names as written (including `xmlns` / `xmlns:p`).
    pub attr_names: Vec<String>,
}

/// Lexes the start tag beginning at `start` (which must point at `<`).
/// Returns `None` if the text does not look like a start tag.
pub(crate) fn start_tag(text: &str, start: usize) -> Option<StartTag> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let is_delim = |b: u8| b.is_ascii_whitespace() || b == b'/' || b == b'>' || b == b'=';
    let mut i = start + 1;
    while i < bytes.len() && !is_delim(bytes[i]) {
        i += 1;
    }
    let name_end = i;
    let mut attr_names = Vec::new();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => return None,
            Some(b'/') | Some(b'>') => break,
            Some(_) => {}
        }
        let n0 = i;
        while i < bytes.len() && !is_delim(bytes[i]) {
            i += 1;
        }
        let name = text.get(n0..i)?.to_owned();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            return None;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let quote = *bytes.get(i)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        i += 1;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        i += 1;
        attr_names.push(name);
    }
    Some(StartTag {
        name_end,
        attr_names,
    })
}

/// A replacement of `range` (possibly empty, i.e. an insertion) by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

/// Applies edits to `source[window]`. Edits outside the window or overlapping an earlier
/// edit are ignored; they cannot occur for ranges taken from one parsed document.
pub(crate) fn splice(source: &str, window: Range<usize>, mut edits: Vec<Edit>) -> String {
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    let mut out = String::with_capacity(window.len() + 64);
    let mut at = window.start;
    for e in edits {
        if e.range.start < at || e.range.end > window.end || e.range.start > e.range.end {
            continue;
        }
        let (Some(keep), true) = (
            source.get(at..e.range.start),
            source.is_char_boundary(e.range.end),
        ) else {
            continue;
        };
        out.push_str(keep);
        out.push_str(&e.text);
        at = e.range.end;
    }
    if let Some(rest) = source.get(at..window.end) {
        out.push_str(rest);
    }
    out
}

/// Finds the value range of `encoding="…"` in a leading XML declaration.
pub(crate) fn xml_decl_encoding(text: &str) -> Option<Range<usize>> {
    if !text.starts_with("<?xml") {
        return None;
    }
    let end = text.find("?>")?;
    let decl = &text[..end];
    let at = decl.find("encoding")?;
    let mut i = at + "encoding".len();
    let b = decl.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if b.get(i) != Some(&b'=') {
        return None;
    }
    i += 1;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let q = *b.get(i)?;
    if q != b'"' && q != b'\'' {
        return None;
    }
    let vstart = i + 1;
    let vend = vstart + decl.get(vstart..)?.find(q as char)?;
    Some(vstart..vend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::pos_at_byte;

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
    fn lexes_start_tag() {
        let t = "x <xs:schema xmlns:xs='u' a = \"v>w\"\n targetNamespace=\"t\"><a/>";
        let st = start_tag(t, 2).expect("start tag");
        assert_eq!(&t[2..st.name_end], "<xs:schema");
        assert_eq!(st.attr_names, ["xmlns:xs", "a", "targetNamespace"]);
        assert!(start_tag(t, 0).is_none());
        assert!(start_tag("<a b='x", 0).is_none());
        let st = start_tag("<a/>", 0).expect("empty");
        assert_eq!(st.name_end, 2);
    }

    #[test]
    fn splices_edits_in_window() {
        let s = "0123456789";
        let out = splice(
            s,
            2..8,
            vec![
                Edit {
                    range: 5..6,
                    text: "X".into(),
                },
                Edit {
                    range: 3..3,
                    text: "+".into(),
                },
                Edit {
                    range: 9..10,
                    text: "ignored".into(),
                },
            ],
        );
        assert_eq!(out, "2+34X67");
    }

    #[test]
    fn finds_declared_encoding() {
        let t = "<?xml version='1.0' encoding = 'UTF-16'?><a/>";
        let r = xml_decl_encoding(t).expect("found");
        assert_eq!(&t[r], "UTF-16");
        assert!(xml_decl_encoding("<?xml version='1.0'?><a encoding='x'/>").is_none());
        assert!(xml_decl_encoding("<a/>").is_none());
    }
}
