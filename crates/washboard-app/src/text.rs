//! Text helpers of the app that need no AppKit, so their tests run on Linux too.

use std::ops::Range;

use washboard_core::diag::{Diagnostic, Severity};
use washboard_core::wsdl::{Reference, Resolution, Unresolved};
use washboard_ui_model::{
    CheckState, CheckedImport, CompletionKind, Hover, ImportSheet, ReplaceOutcome,
};

/// The byte range of `old` that was replaced, and the length of its replacement in `new`.
/// Any consistent description works for `TokenBuffer::edit`; this one is the smallest.
pub(crate) fn changed_range(old: &str, new: &str) -> (Range<usize>, usize) {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    // Keep both ends on char boundaries.
    let mut start = prefix;
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut old_end = old.len() - suffix;
    let mut new_end = new.len() - suffix;
    while !old.is_char_boundary(old_end) || !new.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    (start..old_end, new_end - start)
}

/// The edit that turns `old` into `new`: the UTF-16 range of `old` to replace and the text to
/// put there. For when the text view changed without saying how.
pub(crate) fn utf16_edit(old: &str, new: &str) -> (Range<usize>, String) {
    let (bytes, new_len) = changed_range(old, new);
    let mut cursor = washboard_core::xml::utf16::Utf16Cursor::new(old);
    let range = cursor.utf16_range(bytes.clone());
    (range, new[bytes.start..bytes.start + new_len].to_owned())
}

/// A row of the import sheet's reference table: mark, the reference as written, and where it
/// resolved to.
pub(crate) fn reference_row(r: &Reference) -> Vec<String> {
    let target = r
        .location
        .as_deref()
        .or(r.namespace.as_deref())
        .unwrap_or("");
    let written = format!("{} {target}", r.kind.label()).trim_end().to_owned();
    let (mark, resolved) = match &r.resolution {
        Resolution::Resolved { file, .. } => ("✓", file.clone()),
        Resolution::ByNamespace => ("✓", "by namespace".to_owned()),
        Resolution::Unresolved(Unresolved::NotSupplied) => ("✗", "not supplied".to_owned()),
        Resolution::Unresolved(Unresolved::NoLocation) => ("✗", "no location".to_owned()),
        Resolution::Unresolved(Unresolved::Ambiguous { candidates }) => {
            ("✗", format!("ambiguous: {}", candidates.join(", ")))
        }
    };
    vec![mark.to_owned(), written, resolved]
}

/// The import sheet's one-line summary under the reference table.
pub(crate) fn import_status(sheet: &ImportSheet) -> String {
    match &sheet.check {
        CheckState::Empty => "Choose the WSDL.".to_owned(),
        CheckState::Checking => "Checking…".to_owned(),
        CheckState::Failed(message) => format!("Could not read the files: {message}"),
        CheckState::Done(checked) => {
            let errors = import_diagnostics(checked)
                .filter(|d| d.severity == Severity::Error)
                .count();
            if errors > 0 {
                return format!("{} to fix first.", count(errors, "error"));
            }
            let files = checked.check.files.iter().filter(|f| f.used).count();
            format!(
                "{}, {} resolved.",
                count(files, "file"),
                count(checked.check.references.len(), "reference")
            )
        }
    }
}

/// The import check's and the schema compile's findings, as rows of mark and message.
pub(crate) fn import_messages(checked: &CheckedImport) -> Vec<Vec<String>> {
    import_diagnostics(checked)
        .map(|d| {
            let mark = match d.severity {
                Severity::Error => "✗",
                Severity::Warning => "⚠",
            };
            vec![mark.to_owned(), d.message.clone()]
        })
        .collect()
}

fn import_diagnostics(checked: &CheckedImport) -> impl Iterator<Item = &Diagnostic> {
    checked.check.diagnostics.iter().chain(&checked.compile)
}

/// What Replace WSDL changed, in one sentence for the alert after it.
pub(crate) fn replace_summary(outcome: &ReplaceOutcome) -> String {
    format!(
        "{} added, {} removed; {} no longer valid.",
        count(outcome.added.len(), "operation"),
        outcome.removed.len(),
        count(outcome.invalid.len(), "request")
    )
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Which completions typing `inserted` after `before` (the character in front of it) opens
/// the list for: elements after `<`, attributes after a space, values after `="`. The
/// model decides whether there are any; a space outside a tag offers no attributes.
pub(crate) fn completion_kinds(before: Option<char>, inserted: &str) -> &'static [CompletionKind] {
    match (before, inserted) {
        (_, "<") => &[CompletionKind::Element],
        (_, " ") => &[CompletionKind::Attribute],
        (Some('='), "\"") => &[CompletionKind::Value, CompletionKind::Type],
        _ => &[],
    }
}

/// The hover tool tip: the element's name, one line per fact, then its documentation.
pub(crate) fn hover_text(hover: &Hover) -> String {
    let mut text = hover.name.clone();
    for line in &hover.lines {
        text.push('\n');
        text.push_str(line);
    }
    if let Some(doc) = &hover.documentation {
        text.push_str("\n\n");
        text.push_str(doc);
    }
    text
}

#[cfg(test)]
mod tests {
    use washboard_core::diag::TextPos;
    use washboard_core::model::RequestId;
    use washboard_core::wsdl::{MatchedBy, RefKind, Reference, Resolution, Unresolved};
    use washboard_ui_model::{CompletionKind, Hover, ReplaceOutcome};

    use super::{
        changed_range, completion_kinds, hover_text, reference_row, replace_summary, utf16_edit,
    };

    #[test]
    fn completion_opens_after_lt_space_and_eq_quote() {
        assert_eq!(completion_kinds(Some('>'), "<"), [CompletionKind::Element]);
        assert_eq!(
            completion_kinds(Some('a'), " "),
            [CompletionKind::Attribute]
        );
        assert_eq!(
            completion_kinds(Some('='), "\""),
            [CompletionKind::Value, CompletionKind::Type]
        );
        assert!(completion_kinds(Some(' '), "\"").is_empty());
        assert!(completion_kinds(Some('a'), "b").is_empty());
        assert!(
            completion_kinds(None, "<a").is_empty(),
            "a paste is not typing"
        );
    }

    #[test]
    fn hover_text_puts_documentation_last() {
        let hover = Hover {
            range: 0..4,
            name: "asOf".into(),
            lines: vec!["type xs:date".into(), "exactly 1".into()],
            documentation: Some("The day to look at.".into()),
        };
        assert_eq!(
            hover_text(&hover),
            "asOf\ntype xs:date\nexactly 1\n\nThe day to look at."
        );
    }

    fn reference(
        kind: RefKind,
        location: Option<&str>,
        namespace: Option<&str>,
        resolution: Resolution,
    ) -> Reference {
        Reference {
            kind,
            from: "CustomerService.wsdl".into(),
            inline_schema: None,
            pos: TextPos { line: 1, column: 1 },
            location: location.map(Into::into),
            namespace: namespace.map(Into::into),
            resolution,
        }
    }

    #[test]
    fn reference_rows_show_how_each_resolved() {
        let resolved = Resolution::Resolved {
            file: "xsd/customer.xsd".into(),
            matched_by: MatchedBy::Path,
        };
        let r = reference(
            RefKind::XsdImport,
            Some("customer.xsd"),
            Some("urn:c"),
            resolved,
        );
        assert_eq!(
            reference_row(&r),
            ["✓", "xs:import customer.xsd", "xsd/customer.xsd"]
        );
        let r = reference(
            RefKind::XsdImport,
            None,
            Some("urn:c"),
            Resolution::ByNamespace,
        );
        assert_eq!(reference_row(&r), ["✓", "xs:import urn:c", "by namespace"]);
        let missing = Resolution::Unresolved(Unresolved::NotSupplied);
        let r = reference(RefKind::XsdInclude, Some("ids.xsd"), None, missing);
        assert_eq!(
            reference_row(&r),
            ["✗", "xs:include ids.xsd", "not supplied"]
        );
        let ambiguous = Resolution::Unresolved(Unresolved::Ambiguous {
            candidates: vec!["a/x.xsd".into(), "b/x.xsd".into()],
        });
        let r = reference(RefKind::XsdInclude, Some("x.xsd"), None, ambiguous);
        assert_eq!(reference_row(&r)[2], "ambiguous: a/x.xsd, b/x.xsd");
        let r = reference(
            RefKind::WsdlImport,
            None,
            None,
            Resolution::Unresolved(Unresolved::NoLocation),
        );
        assert_eq!(reference_row(&r), ["✗", "wsdl:import", "no location"]);
    }

    #[test]
    fn replace_summary_counts_the_changes() {
        let outcome = ReplaceOutcome {
            added: Vec::new(),
            removed: Vec::new(),
            invalid: vec![RequestId::new()],
        };
        assert_eq!(
            replace_summary(&outcome),
            "0 operations added, 0 removed; 1 request no longer valid."
        );
    }

    #[test]
    fn utf16_edit_counts_utf16_units() {
        assert_eq!(utf16_edit("<a>ä</a>", "<a>äb</a>"), (4..4, "b".into()));
        assert_eq!(utf16_edit("😀x", "😀"), (2..3, String::new()));
        assert_eq!(utf16_edit("abc", "abc"), (3..3, String::new()));
    }

    #[test]
    fn changed_range_finds_the_edit() {
        assert_eq!(changed_range("<a></a>", "<ab></a>"), (2..2, 1));
        assert_eq!(changed_range("<ab></a>", "<a></a>"), (2..3, 0));
        assert_eq!(changed_range("abc", "abc"), (3..3, 0));
        assert_eq!(changed_range("", "x"), (0..0, 1));
        // Repeated characters: any consistent answer will do, this one is the smallest.
        assert_eq!(changed_range("aaa", "aaaa"), (3..3, 1));
        // Multi-byte chars stay whole: é and è share their first UTF-8 byte.
        assert_eq!(changed_range("é", "è"), (0..2, 2));
    }
}
