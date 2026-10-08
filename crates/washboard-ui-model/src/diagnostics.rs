//! Live checking of the editor's text (PLAN §4 "Validation"): well-formedness 150 ms after the
//! last edit, full validation 1 s after it, both on workers. Every result names the editor
//! version it was computed from and is dropped if the text has changed since.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use washboard_core::diag::{DiagSource, Diagnostic, LineIndex, Severity};
use washboard_core::model::{OperationRef, RequestId};
use washboard_core::validate::validate_request;
use washboard_core::xml::utf16::byte_to_utf16;
use washboard_core::xml::well_formedness_error;

use crate::app::{App, ModelError, ProjectKey};
use crate::event::Event;
use crate::timers::TimerKind;
use crate::window::{ProjectSchema, SchemaState};

pub(crate) const WELL_FORMED_DELAY: Duration = Duration::from_millis(150);
pub(crate) const VALIDATE_DELAY: Duration = Duration::from_secs(1);

/// One row of the issues list, positioned for the text widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub source: DiagSource,
    pub message: String,
    /// 1-based, for the issues list. `None` for problems without a place ("no operation
    /// matches").
    pub line: Option<u32>,
    /// What to underline, in UTF-16 units of the editor's text. Empty when only a position is
    /// known.
    pub range: Option<Range<usize>>,
}

impl Issue {
    pub(crate) fn new(text: &str, lines: &LineIndex<'_>, d: Diagnostic) -> Issue {
        let utf16 = |pos| lines.byte(pos).map(|b| byte_to_utf16(text, b));
        let range = match (d.span, d.pos) {
            (Some(span), _) => utf16(span.start).zip(utf16(span.end)).map(|(s, e)| s..e),
            (None, Some(pos)) => utf16(pos).map(|at| at..at),
            (None, None) => None,
        };
        Issue {
            severity: d.severity,
            source: d.source,
            message: d.message,
            line: d.pos.map(|p| p.line),
            range,
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// What the editor's issues are based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuesBasis {
    /// The text changed since the last full check, or the WSDL is still loading. Only a
    /// well-formedness result may be current.
    Pending,
    /// Validated against the schema; no errors means the request is valid.
    Validated,
    /// Checked for well-formedness only, because the WSDL or its schemas failed to load.
    WellFormedOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// Replaces the well-formedness issue and keeps the rest until the full check catches up.
    WellFormed,
    /// Replaces all issues.
    Full,
}

/// Which schema a full check can use.
enum Against {
    Schema(Arc<ProjectSchema>),
    /// Only well-formedness, with this warning saying why.
    WellFormedOnly(String),
}

impl App {
    /// The Validate command: checks right away instead of waiting for the debounce.
    pub fn validate(&mut self, key: ProjectKey) -> Result<(), ModelError> {
        let window = self.window(key)?;
        if window.editor.is_none() {
            return Err(ModelError::NoRequestSelected);
        }
        self.start_check(key, Check::Full);
        Ok(())
    }

    /// After an edit: restarts both debounce timers.
    pub(crate) fn schedule_checks(&mut self, key: ProjectKey) {
        self.restart_timer(key, TimerKind::WellFormed, WELL_FORMED_DELAY);
        self.restart_timer(key, TimerKind::Validate, VALIDATE_DELAY);
    }

    /// Checks the editor's current text on a worker. A full check waits while the WSDL
    /// loads; loading the schema starts it then.
    pub(crate) fn start_check(&mut self, key: ProjectKey, check: Check) {
        self.stop_timer(key, TimerKind::WellFormed);
        if check == Check::Full {
            self.stop_timer(key, TimerKind::Validate);
        }
        let Some(window) = self.window_mut(key) else {
            return;
        };
        let Some(editor) = &window.editor else {
            return;
        };
        let request = editor.request();
        let version = editor.version();
        let text = editor.text().to_owned();
        match check {
            Check::WellFormed => self.spawn(
                move || {
                    let issue = well_formedness_error(&text)
                        .map(|e| Issue::new(&text, &LineIndex::new(&text), e.diagnostic));
                    issue.into_iter().collect()
                },
                move |app, issues| app.check_done(key, request, version, check, issues),
            ),
            Check::Full => {
                let against = match &window.schema {
                    SchemaState::Loading => return,
                    SchemaState::Ready(schema) if schema.compile_errors().is_empty() => {
                        Against::Schema(schema.clone())
                    }
                    SchemaState::Ready(_) => Against::WellFormedOnly(
                        "The WSDL's schemas could not be compiled; only well-formedness is \
                         checked."
                            .into(),
                    ),
                    SchemaState::Failed(m) => Against::WellFormedOnly(format!(
                        "The WSDL could not be loaded ({m}); only well-formedness is checked."
                    )),
                };
                let hint = window
                    .project
                    .request(request)
                    .ok()
                    .and_then(|r| r.operation);
                self.spawn(
                    move || full_check(&text, &against, hint.as_ref()),
                    move |app, issues| app.check_done(key, request, version, check, issues),
                );
            }
        }
    }

    pub(crate) fn check_done(
        &mut self,
        key: ProjectKey,
        request: RequestId,
        version: u64,
        check: Check,
        issues: Vec<Issue>,
    ) {
        let Some(window) = self.window_mut(key) else {
            return;
        };
        // A schema that changed since the check started starts another check, which replaces
        // this result, so the state now is good enough.
        let schema = window.schema.validating().is_ok();
        let Some(editor) = window.editor.as_mut() else {
            return;
        };
        if editor.request() != request || editor.version() != version {
            return;
        }
        if check == Check::Full {
            editor.checked = Some((version, schema));
        }
        match check {
            Check::WellFormed => {
                editor
                    .issues
                    .retain(|i| i.source != DiagSource::WellFormedness);
                editor.issues.splice(0..0, issues);
            }
            Check::Full => editor.issues = issues,
        }
        let invalid = editor.issues.iter().any(Issue::is_error);
        let marker_changed = window
            .sidebar
            .requests
            .iter_mut()
            .find(|r| r.id == request)
            .is_some_and(|row| std::mem::replace(&mut row.invalid, invalid) != invalid);
        self.events.push(Event::DiagnosticsChanged { project: key });
        if marker_changed {
            self.events.push(Event::SidebarChanged { project: key });
        }
    }
}

fn full_check(text: &str, against: &Against, hint: Option<&OperationRef>) -> Vec<Issue> {
    let diagnostics = match against {
        Against::Schema(schema) => match schema.request_schema() {
            Some(request_schema) => {
                validate_request(&schema.wsdl, &request_schema, text, hint).diagnostics
            }
            None => Vec::new(),
        },
        Against::WellFormedOnly(why) => {
            let mut out: Vec<_> = well_formedness_error(text)
                .map(|e| e.diagnostic)
                .into_iter()
                .collect();
            out.push(Diagnostic::warning(DiagSource::Schema, None, why.clone()));
            out
        }
    };
    let lines = LineIndex::new(text);
    diagnostics
        .into_iter()
        .map(|d| Issue::new(text, &lines, d))
        .collect()
}
