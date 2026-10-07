//! Request validation: `washboard_core::validate`'s pipeline, and the one place `send` goes
//! through before it sends anything.

use std::path::Path;
use std::process::ExitCode;

use anyhow::bail;
use washboard_core::diag::{self, Diagnostic, Severity};
use washboard_core::model::RequestMeta;
use washboard_core::validate::{self, RequestSchema, Validation};
use washboard_core::wsdl::Wsdl;

use crate::support;

/// Validates `text` (the request's content) against the project's WSDL and prints every
/// diagnostic, warnings included, to stderr. Whether the request may be sent is
/// [`Validation::has_errors`]; the `Err` case means the project itself is broken.
///
/// The schema is compiled per call: the CLI is one command per process, where the app
/// compiles once in the background when the project opens.
pub fn check(wsdl: &Wsdl, request: &RequestMeta, text: &str) -> anyhow::Result<Validation> {
    let schema = match RequestSchema::compile(&wsdl.bundle) {
        Ok(s) => s,
        Err(diagnostics) => {
            for d in &diagnostics {
                eprintln!("{d}");
            }
            bail!("the project's schemas do not compile");
        }
    };
    let result = validate::validate_request(wsdl, &schema, text, request.operation.as_ref());
    for d in &result.diagnostics {
        eprint!("{}", render(&request.name, text, d));
    }
    Ok(result)
}

/// A diagnostic as rustc prints one: message, location, and the source line with a caret
/// under the column, followed by a blank line.
///
/// Diagnostics carry a start position only, so the caret marks where the problem starts (for
/// schema errors, the `<` of the element) rather than underlining a span. PLAN §5.2 has what
/// a span would need.
fn render(name: &str, text: &str, d: &Diagnostic) -> String {
    let severity = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut out = format!("{severity}: {}\n", d.message);
    let Some(pos) = d.pos else {
        out.push_str(&format!("  --> {name}\n\n"));
        return out;
    };
    out.push_str(&format!("  --> {name}:{}:{}\n", pos.line, pos.column));
    let Some(line) = usize::try_from(pos.line)
        .ok()
        .and_then(|l| text.lines().nth(l.saturating_sub(1)))
    else {
        out.push('\n');
        return out;
    };
    let number = pos.line.to_string();
    let gutter = " ".repeat(number.len());
    // Keep tabs so the caret lines up however the terminal expands them.
    let indent: String = line
        .chars()
        .take(usize::try_from(pos.column).unwrap_or(1).saturating_sub(1))
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    out.push_str(&format!(
        "{gutter} |\n{number} | {line}\n{gutter} | {indent}^\n\n"
    ));
    out
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
    use washboard_core::diag::{DiagSource, TextPos};

    use super::*;

    #[test]
    fn excerpt_puts_the_caret_under_the_column() {
        let text = "<a>\n\t  <b>x</b>\n</a>\n";
        let d = Diagnostic::error(
            DiagSource::Schema,
            Some(TextPos { line: 2, column: 4 }),
            "bad b",
        );
        assert_eq!(
            render("Req 1", text, &d),
            "error: bad b\n  --> Req 1:2:4\n  |\n2 | \t  <b>x</b>\n  | \t  ^\n\n"
        );
    }

    #[test]
    fn no_position_or_line_out_of_range_prints_no_excerpt() {
        let d = Diagnostic::warning(DiagSource::Soap, None, "w");
        assert_eq!(render("R", "<a/>", &d), "warning: w\n  --> R\n\n");
        let d = Diagnostic::error(DiagSource::Soap, Some(TextPos { line: 9, column: 1 }), "e");
        assert_eq!(render("R", "<a/>", &d), "error: e\n  --> R:9:1\n\n");
    }
}
