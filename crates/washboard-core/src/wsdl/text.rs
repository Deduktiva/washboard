//! Splicing edits into source text.
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
}
