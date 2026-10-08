//! Completion and hover in the editor (PLAN §4 "Editor"), from the project's shared
//! `SchemaModel`. Both run on the main thread: they scan the text once up to the cursor and
//! ask the model a few questions, which is cheap next to a keystroke's redraw.

use std::ops::Range;

use washboard_core::model::QName;
use washboard_core::schema::{EnumValue, MaxOccurs, SchemaModel, SuggestionSource, block_path};
use washboard_core::soap::XSI_NS;
use washboard_core::xml::utf16::{Utf16Cursor, utf16_to_byte};
use washboard_core::xml::{CursorContext, CursorLocation, NamespaceMap, cursor_context};

use crate::app::{App, ProjectKey};
use crate::window::SchemaState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Element,
    Attribute,
    Value,
    /// An `xsi:type` value.
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// What to insert, with the prefix in scope at the cursor.
    pub text: String,
    pub kind: CompletionKind,
    /// Short context for the list: type and cardinality, "required", ….
    pub detail: Option<String>,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completions {
    /// The UTF-16 range the chosen item replaces (the part already typed).
    pub replace: Range<usize>,
    pub items: Vec<CompletionItem>,
}

/// What the hover popover shows for the element under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    /// UTF-16 range of the element name the hover is about.
    pub range: Range<usize>,
    pub name: String,
    /// One line each: type (and the `xsi:type` in effect), cardinality, flags.
    pub lines: Vec<String>,
    pub documentation: Option<String>,
}

impl App {
    /// Completions at UTF-16 offset `at` in the project's editor. `None` where there is
    /// nothing to offer (no schema yet, outside a header or body block, in a comment, …).
    pub fn completions(&self, key: ProjectKey, at: usize) -> Option<Completions> {
        let (text, model) = self.assist_input(key)?;
        let byte = utf16_to_byte(text, at);
        let ctx = cursor_context(text, byte);
        let span = |r: Range<usize>| Utf16Cursor::new(text).utf16_range(r);
        let scope = ctx.path.last().map(|e| &e.namespaces)?;
        let (replace, items) = match &ctx.location {
            CursorLocation::ElementName {
                closing: false,
                name,
            } => {
                let path = block_path(ctx.parent_path())?;
                let items = model
                    .child_elements(&path)
                    .elements
                    .into_iter()
                    .map(|c| CompletionItem {
                        text: element_text(&c.name, parent_scope(&ctx).unwrap_or(scope)),
                        kind: CompletionKind::Element,
                        detail: Some(element_detail(
                            c.type_name.as_ref(),
                            c.min_occurs,
                            c.max_occurs,
                            &c.source,
                        )),
                        documentation: c.documentation,
                    })
                    .collect();
                (span(name.clone()), items)
            }
            CursorLocation::ElementName {
                closing: true,
                name,
            } => {
                // In a tag, the path ends with the element the tag belongs to: for an end tag,
                // the one it closes.
                let open = ctx.path.last()?;
                let item = CompletionItem {
                    text: open.raw_name.clone(),
                    kind: CompletionKind::Element,
                    detail: None,
                    documentation: None,
                };
                (span(name.clone()), vec![item])
            }
            CursorLocation::AttributeName { name } => {
                let path = block_path(&ctx.path)?;
                let items = model
                    .attributes(&path)
                    .attributes
                    .into_iter()
                    .filter_map(|a| {
                        Some(CompletionItem {
                            text: attribute_text(&a.name, scope)?,
                            kind: CompletionKind::Attribute,
                            detail: Some(if a.required { "required" } else { "optional" }.into()),
                            documentation: a.documentation,
                        })
                    })
                    .collect();
                (span(name.clone()), items)
            }
            CursorLocation::AttributeValue {
                name: Some(attribute),
                value,
                ..
            } => {
                let path = block_path(&ctx.path)?;
                let items = if attribute.ns == XSI_NS && attribute.local == "type" {
                    model
                        .xsi_type_candidates(&path)
                        .into_iter()
                        .map(|t| CompletionItem {
                            text: element_text(&t.name, scope),
                            kind: CompletionKind::Type,
                            detail: t.is_declared.then(|| "declared type".to_owned()),
                            documentation: t.documentation,
                        })
                        .collect()
                } else {
                    values(model.attribute_values(&path, attribute))
                };
                (span(value.clone()), items)
            }
            CursorLocation::Text => {
                let path = block_path(&ctx.path)?;
                (at..at, values(model.text_values(&path)))
            }
            _ => return None,
        };
        Some(Completions { replace, items })
    }

    /// Hover for the element whose name is under UTF-16 offset `at`.
    pub fn hover(&self, key: ProjectKey, at: usize) -> Option<Hover> {
        let (text, model) = self.assist_input(key)?;
        let ctx = cursor_context(text, utf16_to_byte(text, at));
        let CursorLocation::ElementName { name, .. } = &ctx.location else {
            return None;
        };
        let element = ctx.path.last()?;
        let info = model.element_info(&block_path(&ctx.path)?)?;
        let written = |q: &QName| element_text(q, &element.namespaces);
        let mut lines = Vec::new();
        match (&info.declared_type, &info.actual_type) {
            (Some(declared), Some(actual)) if declared != actual => lines.push(format!(
                "type {} (declared {})",
                written(actual),
                written(declared)
            )),
            (Some(declared), _) => lines.push(format!("type {}", written(declared))),
            (None, _) => {}
        }
        lines.push(cardinality(info.min_occurs, info.max_occurs));
        if info.is_abstract {
            lines.push("abstract element: use a substitution group member".into());
        }
        if info.type_is_abstract {
            lines.push("abstract type: needs xsi:type".into());
        }
        if info.nillable {
            lines.push("nillable".into());
        }
        Some(Hover {
            range: Utf16Cursor::new(text).utf16_range(name.clone()),
            name: element.raw_name.clone(),
            lines,
            documentation: info.documentation,
        })
    }

    fn assist_input(&self, key: ProjectKey) -> Option<(&str, &SchemaModel)> {
        let window = self.project(key)?;
        let SchemaState::Ready(schema) = window.schema() else {
            return None;
        };
        Some((window.editor()?.text(), &*schema.model))
    }
}

/// Namespaces in scope where a new child element is typed (the parent's, not those of a tag
/// the cursor may be in).
fn parent_scope(ctx: &CursorContext) -> Option<&NamespaceMap> {
    ctx.parent_path().last().map(|e| &e.namespaces)
}

/// `prefix:local`, `local` in the default namespace, or `local xmlns="…"` when no prefix is
/// bound, so the inserted element is in the right namespace either way.
fn element_text(name: &QName, scope: &NamespaceMap) -> String {
    match scope.prefix_for(&name.ns) {
        Some("") => name.local.clone(),
        Some(prefix) => format!("{prefix}:{}", name.local),
        None if name.ns.is_empty() => name.local.clone(),
        None => format!("{} xmlns=\"{}\"", name.local, name.ns),
    }
}

/// Attributes can't use the default namespace; one whose namespace has no prefix in scope is
/// left out rather than offered in a form that would mean something else.
fn attribute_text(name: &QName, scope: &NamespaceMap) -> Option<String> {
    if name.ns.is_empty() {
        return Some(name.local.clone());
    }
    match scope.prefix_for(&name.ns) {
        Some("") | None => None,
        Some(prefix) => Some(format!("{prefix}:{}", name.local)),
    }
}

fn values(values: Vec<EnumValue>) -> Vec<CompletionItem> {
    values
        .into_iter()
        .map(|v| CompletionItem {
            text: v.value,
            kind: CompletionKind::Value,
            detail: None,
            documentation: v.documentation,
        })
        .collect()
}

fn element_detail(
    type_name: Option<&QName>,
    min: u32,
    max: MaxOccurs,
    source: &SuggestionSource,
) -> String {
    let mut out = cardinality(min, max);
    if let Some(t) = type_name {
        out = format!("{}, {out}", t.local);
    }
    if let SuggestionSource::Substitution { head } = source {
        out.push_str(&format!(", for {}", head.local));
    }
    out
}

fn cardinality(min: u32, max: MaxOccurs) -> String {
    match max {
        MaxOccurs::Bounded(max) if min == max => format!("exactly {min}"),
        MaxOccurs::Bounded(max) => format!("{min}..{max}"),
        MaxOccurs::Unbounded => format!("{min}..*"),
    }
}
