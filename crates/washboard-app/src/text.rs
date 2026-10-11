//! Text helpers of the app that need no AppKit, so their tests run on Linux too.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use washboard_core::diag::{Diagnostic, Severity};
use washboard_core::model::OperationRef;
use washboard_core::wsdl::{Reference, Resolution, Unresolved};
use washboard_ui_model::{
    CheckState, CheckedImport, CompletionKind, Hover, ImportSheet, ImportTarget, Issue,
    ReplaceOutcome, WellFormedness,
};

/// A row of the import sheet's reference table: mark, the file the reference is in, the
/// reference as written, and where it resolved to. The same file is often imported from
/// several places, so without the importer two rows can look identical.
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
        Resolution::Unresolved(Unresolved::NotSupplied) => ("✗", "not found".to_owned()),
        Resolution::Unresolved(Unresolved::NoLocation) => ("✗", "no location".to_owned()),
        Resolution::Unresolved(Unresolved::Ambiguous { candidates }) => {
            ("✗", format!("ambiguous: {}", candidates.join(", ")))
        }
    };
    vec![mark.to_owned(), r.from.clone(), written, resolved]
}

/// The import sheet's list of other files: what the user added (not the WSDL itself, which
/// has its own row), then how many were found next to the files that reference them.
pub(crate) fn import_files(sheet: &ImportSheet) -> String {
    let added: Vec<String> = sheet
        .extra
        .iter()
        .filter(|p| Some(*p) != sheet.entry.as_ref())
        .map(|p| file_name(p))
        .collect();
    let found = match &sheet.check {
        CheckState::Done(checked) => checked.found.len(),
        _ => 0,
    };
    let found = (found > 0).then(|| format!("{} found next to the WSDL", count(found, "file")));
    match (added.is_empty(), found) {
        (true, None) => "None".to_owned(),
        (true, Some(found)) => capitalize(&found),
        (false, None) => added.join(", "),
        (false, Some(found)) => format!("{}; {found}", added.join(", ")),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The import sheet's one-line summary under the reference table; while Create is disabled
/// it says why.
pub(crate) fn import_status(sheet: &ImportSheet, target: ImportTarget) -> String {
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
            if target == ImportTarget::NewProject {
                match (sheet.name.trim().is_empty(), sheet.parent.is_none()) {
                    (true, true) => return "Name the project and choose its location.".into(),
                    (true, false) => return "Name the project.".into(),
                    (false, true) => return "Choose the project's location.".into(),
                    (false, false) => {}
                }
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

/// `1 error`, `2 errors`.
pub(crate) fn count(n: usize, noun: &str) -> String {
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

/// A gutter marker: a line with issues, and the tool tip that lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineMarker {
    /// 1-based.
    pub line: usize,
    /// Any error on the line marks it as an error; a line with only warnings, as a warning.
    pub error: bool,
    /// The issues' messages, errors first, each in the order the issues came.
    pub tool_tip: String,
}

/// One marker per line with issues, sorted by line. Issues without a line have no place in
/// the gutter; the issues list still shows them.
pub(crate) fn line_markers(issues: &[Issue]) -> Vec<LineMarker> {
    let mut lines: BTreeMap<usize, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
    for issue in issues {
        let Some(line) = issue.line else {
            continue;
        };
        let (errors, warnings) = lines.entry(line as usize).or_default();
        if issue.is_error() {
            errors.push(&issue.message);
        } else {
            warnings.push(&issue.message);
        }
    }
    lines
        .into_iter()
        .map(|(line, (errors, warnings))| LineMarker {
            line,
            error: !errors.is_empty(),
            tool_tip: errors
                .into_iter()
                .chain(warnings)
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect()
}

/// The text's tool tip at UTF-16 offset `at`, where the mouse rests: the messages of the
/// underlined issues there, then the element hover after a blank line, and the range the tool
/// tip covers. The first underline at `at` sets the range; with a hover it narrows to where
/// both apply, so moving onto another name asks again. `underlines` are the ranges as drawn,
/// never empty; `at` on a range's end counts, as an insertion point right after its last
/// character.
pub(crate) fn text_tool_tip(
    underlines: &[(Range<usize>, String)],
    hover: Option<(Range<usize>, String)>,
    at: usize,
) -> Option<(Range<usize>, String)> {
    let mut covering = underlines
        .iter()
        .filter(|(range, _)| range.start <= at && at <= range.end);
    let Some((first, _)) = covering.clone().next() else {
        return hover;
    };
    let mut text = covering
        .by_ref()
        .map(|(_, message)| message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut range = first.clone();
    if let Some((hover_range, hover_text)) = hover {
        let both = range.start.max(hover_range.start)..range.end.min(hover_range.end);
        if !both.is_empty() {
            range = both;
            text.push_str("\n\n");
            text.push_str(&hover_text);
        }
    }
    Some((range, text))
}

/// The request bar's operation chip. Always SOAP 1.1: that is the only envelope Washboard
/// sends, whatever else the WSDL offers.
pub(crate) fn operation_chip(operation: &OperationRef) -> String {
    format!("SOAP 1.1 · {}", operation.operation)
}

/// The request bar's well-formedness indicator; empty until the first check has finished.
pub(crate) fn well_formedness_text(state: WellFormedness) -> String {
    match state {
        WellFormedness::Pending => String::new(),
        WellFormedness::WellFormed => "Well-formed".to_owned(),
        WellFormedness::Error { line: Some(line) } => format!("XML error, line {line}"),
        WellFormedness::Error { line: None } => "XML error".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use washboard_core::diag::{DiagSource, Severity, TextPos};
    use washboard_core::model::{OperationRef, QName, RequestId};
    use washboard_core::wsdl::{MatchedBy, RefKind, Reference, Resolution, Unresolved};
    use washboard_ui_model::{
        CompletionKind, Hover, ImportSheet, ImportTarget, Issue, ReplaceOutcome, WellFormedness,
    };

    use super::{
        LineMarker, completion_kinds, hover_text, import_files, import_status, line_markers,
        operation_chip, reference_row, replace_summary, text_tool_tip, well_formedness_text,
    };

    #[test]
    fn the_request_bar_names_the_operation_and_the_xml_state() {
        let operation = OperationRef {
            binding: QName::new("urn:example:legacy", "LegacyBinding"),
            operation: "Lookup".into(),
        };
        assert_eq!(operation_chip(&operation), "SOAP 1.1 · Lookup");
        assert_eq!(well_formedness_text(WellFormedness::Pending), "");
        assert_eq!(
            well_formedness_text(WellFormedness::WellFormed),
            "Well-formed"
        );
        assert_eq!(
            well_formedness_text(WellFormedness::Error { line: Some(9) }),
            "XML error, line 9"
        );
        assert_eq!(
            well_formedness_text(WellFormedness::Error { line: None }),
            "XML error"
        );
    }

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
            [
                "✓",
                "CustomerService.wsdl",
                "xs:import customer.xsd",
                "xsd/customer.xsd"
            ]
        );
        let r = reference(
            RefKind::XsdImport,
            None,
            Some("urn:c"),
            Resolution::ByNamespace,
        );
        assert_eq!(
            reference_row(&r),
            [
                "✓",
                "CustomerService.wsdl",
                "xs:import urn:c",
                "by namespace"
            ]
        );
        let missing = Resolution::Unresolved(Unresolved::NotSupplied);
        let r = reference(RefKind::XsdInclude, Some("ids.xsd"), None, missing);
        assert_eq!(
            reference_row(&r),
            [
                "✗",
                "CustomerService.wsdl",
                "xs:include ids.xsd",
                "not found"
            ]
        );
        let ambiguous = Resolution::Unresolved(Unresolved::Ambiguous {
            candidates: vec!["a/x.xsd".into(), "b/x.xsd".into()],
        });
        let r = reference(RefKind::XsdInclude, Some("x.xsd"), None, ambiguous);
        assert_eq!(reference_row(&r)[3], "ambiguous: a/x.xsd, b/x.xsd");
        let r = reference(
            RefKind::WsdlImport,
            None,
            None,
            Resolution::Unresolved(Unresolved::NoLocation),
        );
        assert_eq!(
            reference_row(&r),
            ["✗", "CustomerService.wsdl", "wsdl:import", "no location"]
        );
    }

    #[test]
    fn the_file_list_leaves_out_the_wsdl() {
        let mut sheet = ImportSheet::default();
        assert_eq!(import_files(&sheet), "None");
        sheet.entry = Some("/w/S.wsdl".into());
        sheet.extra = vec!["/w/S.wsdl".into(), "/w/xsd".into(), "/w/a.xsd".into()];
        assert_eq!(import_files(&sheet), "xsd, a.xsd");
        assert_eq!(
            import_status(&sheet, ImportTarget::NewProject),
            "Choose the WSDL."
        );
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
    fn line_markers_list_errors_first_and_mark_warnings() {
        let issue = |severity, line, message: &str| Issue {
            severity,
            source: DiagSource::Schema,
            message: message.into(),
            line,
            range: None,
        };
        let issues = [
            issue(Severity::Warning, Some(3), "w3"),
            issue(Severity::Error, Some(5), "e5"),
            issue(Severity::Warning, Some(5), "w5"),
            issue(Severity::Error, Some(5), "e5b"),
            issue(Severity::Error, None, "nowhere"),
            issue(Severity::Error, Some(1), "e1"),
        ];
        let marker = |line, error, tool_tip: &str| LineMarker {
            line,
            error,
            tool_tip: tool_tip.into(),
        };
        assert_eq!(
            line_markers(&issues),
            [
                marker(1, true, "e1"),
                marker(3, false, "w3"),
                marker(5, true, "e5\ne5b\nw5"),
            ]
        );
    }

    #[test]
    fn the_text_tool_tip_puts_issues_before_the_hover() {
        let underlines = [
            (5..9, "bad date".to_owned()),
            (7..20, "too long".to_owned()),
        ];
        let hover = |range: std::ops::Range<usize>| Some((range, "asOf\ntype xs:date".to_owned()));
        assert_eq!(text_tool_tip(&underlines, None, 2), None);
        assert_eq!(
            text_tool_tip(&underlines, hover(0..4), 2),
            Some((0..4, "asOf\ntype xs:date".into()))
        );
        assert_eq!(
            text_tool_tip(&underlines, None, 9),
            Some((5..9, "bad date\ntoo long".into())),
            "the end of an underline counts"
        );
        assert_eq!(
            text_tool_tip(&underlines, hover(6..12), 8),
            Some((6..9, "bad date\ntoo long\n\nasOf\ntype xs:date".into()))
        );
        assert_eq!(
            text_tool_tip(&underlines, hover(9..12), 9),
            Some((5..9, "bad date\ntoo long".into())),
            "a hover that only touches the underline stays out"
        );
    }
}
