//! Request validation: `washboard_core::validate`'s pipeline, and the one place `send` goes
//! through before it sends anything.

use std::io::IsTerminal;
use std::ops::Range;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use annotate_snippets::{AnnotationKind, Level, Origin, Renderer, Snippet};
use anyhow::bail;
use washboard_core::diag::{self, Diagnostic, LineIndex, Severity};
use washboard_core::model::RequestMeta;
use washboard_core::schema::SchemaModel;
use washboard_core::validate::{self, RequestSchema, Validation};
use washboard_core::wsdl::Wsdl;

use crate::support;

/// Validates `text` (the request's content) against the project's WSDL and prints every
/// diagnostic, warnings included, to stderr. Whether the request may be sent is
/// [`Validation::has_errors`]; the `Err` case means the project itself is broken.
///
/// The schema (and the model that explains abstract-type errors) is built per call: the CLI is
/// one command per process, where the app builds both once in the background when the project
/// opens.
pub fn check(wsdl: &Wsdl, request: &RequestMeta, text: &str) -> anyhow::Result<Validation> {
    let schema = match RequestSchema::compile(&wsdl.bundle) {
        Ok(s) => s.with_model(Arc::new(SchemaModel::build(&wsdl.bundle))),
        Err(diagnostics) => {
            for d in &diagnostics {
                eprintln!("{d}");
            }
            bail!("the project's schemas do not compile");
        }
    };
    let result = validate::validate_request(wsdl, &schema, text, request.operation.as_ref());
    let renderer = if std::io::stderr().is_terminal() {
        Renderer::styled()
    } else {
        Renderer::plain()
    };
    for d in &result.diagnostics {
        eprint!("{}", render(&renderer, &request.name, text, d));
    }
    Ok(result)
}

/// A diagnostic as rustc prints one, followed by a blank line: message, location, and the
/// source lines with the diagnostic's span underlined. Without a span a single caret marks
/// `pos` (for schema errors, the `<` of the element).
fn render(renderer: &Renderer, name: &str, text: &str, d: &Diagnostic) -> String {
    let level = match d.severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
    };
    let title = level.primary_title(d.message.as_str());
    let lines = LineIndex::new(text);
    let range = match (d.span, d.pos) {
        (Some(span), _) => lines
            .byte(span.start)
            .zip(lines.byte(span.end))
            .filter(|(start, end)| start <= end)
            .map(|(start, end)| start..end),
        (None, Some(pos)) => lines.byte(pos).map(|b| point(text, b)),
        (None, None) => None,
    };
    let report = match (range, d.pos) {
        (Some(range), _) => [title.element(
            Snippet::source(text)
                .path(name)
                .annotation(AnnotationKind::Primary.span(range)),
        )],
        (None, Some(pos)) => [title.element(
            Origin::path(name)
                .line(usize::try_from(pos.line).unwrap_or(usize::MAX))
                .char_column(usize::try_from(pos.column).unwrap_or(1)),
        )],
        (None, None) => [title.element(Origin::path(name))],
    };
    format!("{}\n\n", renderer.render(&report))
}

/// The char at `byte`, as a range a caret can mark.
fn point(text: &str, byte: usize) -> Range<usize> {
    let len = text[byte..].chars().next().map_or(0, char::len_utf8);
    byte..byte + len
}

pub fn command(dir: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let project = support::open_read(dir)?;
    let request = support::find_request(&project, name)?;
    let text = project.read_request(request.id)?;
    let wsdl = support::load_project_wsdl(&project)?;
    let result = check(&wsdl, &request, &text)?;
    match diag::error_count(&result.diagnostics) {
        0 => {
            println!("{name}: valid");
            Ok(ExitCode::SUCCESS)
        }
        n => {
            eprintln!("{name}: {n} error{}", if n == 1 { "" } else { "s" });
            Ok(ExitCode::from(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use washboard_core::diag::{DiagSource, TextPos, TextSpan};

    use super::*;

    fn plain(name: &str, text: &str, d: &Diagnostic) -> String {
        render(&Renderer::plain(), name, text, d)
    }

    #[test]
    fn excerpt_puts_the_caret_under_the_column() {
        let text = "<a>\n  ü<b>x</b>\n</a>\n";
        let d = Diagnostic::error(
            DiagSource::Schema,
            Some(TextPos { line: 2, column: 4 }),
            "bad b",
        );
        assert_eq!(
            plain("Req 1", text, &d),
            "error: bad b\n --> Req 1:2:4\n  |\n2 |   ü<b>x</b>\n  |    ^\n\n"
        );
    }

    #[test]
    fn excerpt_underlines_the_span() {
        let text = "<a>\n  <b c=\"x\">x</b>\n</a>\n";
        let p = |line, column| TextPos { line, column };
        let d = Diagnostic::error(DiagSource::Schema, Some(p(2, 3)), "bad c").with_span(TextSpan {
            start: p(2, 6),
            end: p(2, 11),
        });
        assert_eq!(
            plain("R", text, &d),
            "error: bad c\n --> R:2:6\n  |\n2 |   <b c=\"x\">x</b>\n  |      ^^^^^\n\n"
        );
    }

    #[test]
    fn no_position_or_line_out_of_range_prints_no_excerpt() {
        let d = Diagnostic::warning(DiagSource::Soap, None, "w");
        assert_eq!(plain("R", "<a/>", &d), "warning: w\n --> R\n\n");
        let d = Diagnostic::error(DiagSource::Soap, Some(TextPos { line: 9, column: 1 }), "e");
        assert_eq!(plain("R", "<a/>", &d), "error: e\n --> R:9:1\n\n");
    }
}
