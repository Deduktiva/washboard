//! "Format XML" (⌃I).
//!
//! Re-indents element-only content with two spaces per level. Anything that might carry
//! meaning is kept byte for byte: an element that contains character data, CDATA or
//! references (mixed or text content, including whitespace-only leaf content such as
//! `<a> </a>`) is copied verbatim from its start tag's `>` through its end tag. Comments, PIs,
//! the XML declaration, DOCTYPE, namespace declarations and attribute values (with their quotes
//! and escapes) are kept as written. Attributes that started on their own line stay on their
//! own line, aligned with the first attribute.
//!
//! Iterative, not recursive: a pathologically deep document must not overflow the stack.

use crate::diag::Diagnostic;

use super::lex::{Construct, Lexer, RawAttr, TagInfo};
use super::names::is_xml_ws;
use super::wellformed::check_well_formed;

/// Pretty-prints a well-formed document; returns the well-formedness error otherwise.
///
/// Uses `\r\n` line breaks if the input contains any, `\n` otherwise; ends with a line break.
pub fn pretty_print(text: &str) -> Result<String, Diagnostic> {
    check_well_formed(text)?;
    let infos = classify(text);
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut lexer = Lexer::new(text, 0);
    let mut depth = 0usize;
    let mut ordinal = 0usize;
    while let Some(con) = lexer.next_construct() {
        match con {
            // Only reached in element-only content or outside the root: whitespace.
            Construct::Text(_) => {}
            Construct::StartTag(t) => {
                line(&mut out, nl, depth);
                write_start_tag(&mut out, text, nl, depth, &t, &lexer.attrs);
                let info = infos.get(ordinal).copied().unwrap_or_default();
                ordinal += 1;
                if t.self_closing {
                    continue;
                }
                if info.verbatim {
                    let end = info.end.clamp(t.span.end, text.len());
                    out.push_str(&text[t.span.end..end]);
                    ordinal += info.descendants;
                    lexer = Lexer::new(text, end);
                } else {
                    depth += 1;
                }
            }
            Construct::EndTag(t) => {
                depth = depth.saturating_sub(1);
                line(&mut out, nl, depth);
                out.push_str("</");
                out.push_str(&text[t.name.clone()]);
                out.push('>');
            }
            other => {
                line(&mut out, nl, depth);
                out.push_str(&text[other.span()]);
            }
        }
    }
    out.push_str(nl);
    Ok(out)
}

fn line(out: &mut String, nl: &str, depth: usize) {
    if !out.is_empty() {
        out.push_str(nl);
    }
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_start_tag(
    out: &mut String,
    text: &str,
    nl: &str,
    depth: usize,
    t: &TagInfo,
    attrs: &[RawAttr],
) {
    let name = &text[t.name.clone()];
    out.push('<');
    out.push_str(name);
    let align = depth * 2 + 1 + name.chars().count() + 1;
    let mut prev_end = t.name.end;
    for (i, a) in attrs.iter().enumerate() {
        let own_line = i > 0 && text[prev_end..a.name.start].contains('\n');
        if own_line {
            out.push_str(nl);
            out.extend(std::iter::repeat_n(' ', align));
        } else {
            out.push(' ');
        }
        out.push_str(&text[a.name.clone()]);
        if let Some(v) = &a.value {
            out.push('=');
            out.push_str(&text[v.clone()]);
            prev_end = v.end;
        } else {
            prev_end = a.name.end;
        }
    }
    out.push_str(if t.self_closing { "/>" } else { ">" });
}

#[derive(Debug, Clone, Copy, Default)]
struct ElementInfo {
    /// Copy the content and end tag as written.
    verbatim: bool,
    /// End of the end tag.
    end: usize,
    /// Number of elements inside, to skip their ordinals when copying verbatim.
    descendants: usize,
}

#[derive(Debug)]
struct Open {
    ordinal: usize,
    has_char_data: bool,
    has_markup: bool,
}

/// One entry per element in document order.
fn classify(text: &str) -> Vec<ElementInfo> {
    let mut infos: Vec<ElementInfo> = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    for con in Lexer::new(text, 0) {
        match con {
            Construct::Text(r) => {
                if let Some(top) = stack.last_mut() {
                    let data = &text.as_bytes()[r];
                    // Whitespace counts as data only until markup shows it is indentation.
                    if data.iter().any(|&b| !is_xml_ws(b)) {
                        top.has_char_data = true;
                    }
                }
            }
            Construct::CData { .. } => {
                if let Some(top) = stack.last_mut() {
                    top.has_char_data = true;
                }
            }
            Construct::StartTag(t) => {
                if let Some(top) = stack.last_mut() {
                    top.has_markup = true;
                }
                let ordinal = infos.len();
                infos.push(ElementInfo {
                    verbatim: true,
                    end: t.span.end,
                    descendants: 0,
                });
                if !t.self_closing {
                    stack.push(Open {
                        ordinal,
                        has_char_data: false,
                        has_markup: false,
                    });
                }
            }
            Construct::EndTag(t) => {
                if let Some(open) = stack.pop() {
                    let descendants = infos.len() - open.ordinal - 1;
                    if let Some(info) = infos.get_mut(open.ordinal) {
                        info.verbatim = open.has_char_data || !open.has_markup;
                        info.end = t.span.end;
                        info.descendants = descendants;
                    }
                }
            }
            Construct::Comment { .. } | Construct::Pi { .. } => {
                if let Some(top) = stack.last_mut() {
                    top.has_markup = true;
                }
            }
            Construct::Doctype { .. } | Construct::Stray(_) => {}
        }
    }
    infos
}
