//! Small text helpers: splicing edits, finding the XML declaration's encoding.
//!
//! Bundle documents are produced by splicing the decoded source text rather than
//! re-serializing a tree, so everything except the rewritten attributes stays byte-identical
//! and libxml2's line numbers still point into the user's files.

use std::ops::Range;

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
