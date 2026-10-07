//! What surrounds the editor cursor: the input for completion and hover.
//!
//! Only the text before the cursor decides the element path, so a document that is broken
//! after the cursor (the normal state while typing) still gives a useful answer. The tag the
//! cursor is in is read to its end, because namespace declarations later in the same start tag
//! apply to its name.

use std::ops::Range;

use crate::model::QName;
use crate::soap::XSI_NS;

use super::lex::{Construct, Lexer, RawAttr, TagInfo};
use super::names::{NamespaceMap, is_xml_ws, split_qname, unescape_lossy, xmlns_prefix};

/// Result of [`cursor_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorContext {
    /// Open elements around the cursor, outermost first. When the cursor is inside a start or
    /// end tag, the element owning that tag is the last entry (see [`Self::parent_path`]).
    pub path: Vec<PathElement>,
    pub location: CursorLocation,
    in_tag: bool,
}

impl CursorContext {
    /// The element whose start or end tag contains the cursor, if any.
    pub fn tag_element(&self) -> Option<&PathElement> {
        if self.in_tag { self.path.last() } else { None }
    }

    /// The path without the element whose tag contains the cursor: the parent in which a new
    /// child element would be inserted, which is what element-name completion needs.
    pub fn parent_path(&self) -> &[PathElement] {
        match (self.in_tag, self.path.split_last()) {
            (true, Some((_, parents))) => parents,
            _ => &self.path,
        }
    }
}

/// One element on the path from the root to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathElement {
    /// The name as written, e.g. `cus:party`.
    pub raw_name: String,
    /// The resolved name; `None` if its prefix is not declared.
    pub name: Option<QName>,
    /// Byte offset of the start tag's `<`.
    pub start: usize,
    /// Bindings in scope at this element, including its own declarations.
    pub namespaces: NamespaceMap,
    /// `xsi:type` on this element, if present.
    pub xsi_type: Option<XsiType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsiType {
    /// The attribute value as written (whitespace-trimmed), e.g. `com:Person`.
    pub raw: String,
    /// The value resolved against the element's namespaces; `None` if the prefix is undeclared.
    pub name: Option<QName>,
}

/// Where in the markup the cursor is. Ranges are byte ranges of the text being typed, so
/// completion knows what to replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorLocation {
    /// In or right after an element name, or right after `<` / `</`. `name` may be empty.
    ElementName {
        closing: bool,
        name: Range<usize>,
    },
    /// In an attribute name, or in whitespace inside a start tag where one could be typed
    /// (`name` is then empty, at the cursor).
    AttributeName {
        name: Range<usize>,
    },
    /// Inside a quoted attribute value. `value` excludes the quotes.
    AttributeValue {
        /// The attribute name as written.
        attribute: String,
        /// The resolved attribute name (unprefixed attributes have no namespace).
        name: Option<QName>,
        value: Range<usize>,
    },
    /// Inside a tag, but not at a place where a name or value goes (e.g. right after a
    /// closing quote).
    Tag,
    /// Character data inside an element.
    Text,
    CData,
    Comment,
    /// Inside a processing instruction, the XML declaration or a DOCTYPE.
    Markup,
    /// Outside the root element (prolog or after the end of the root).
    Outside,
}

#[derive(Debug)]
struct Frame {
    start: usize,
    name: Range<usize>,
    /// Namespace declarations: (attribute name, value without quotes).
    decls: Vec<(Range<usize>, Range<usize>)>,
    /// Prefixed attributes named `type` (possible `xsi:type`): (attribute name, value).
    types: Vec<(Range<usize>, Range<usize>)>,
}

impl Frame {
    fn new(text: &str, tag: &TagInfo, attrs: &[RawAttr]) -> Self {
        let mut decls = Vec::new();
        let mut types = Vec::new();
        for a in attrs {
            let Some(value) = a.content() else { continue };
            let name = &text[a.name.clone()];
            if xmlns_prefix(name).is_some() {
                decls.push((a.name.clone(), value));
            } else if let (Some(_), "type") = split_qname(name) {
                types.push((a.name.clone(), value));
            }
        }
        Self {
            start: tag.span.start,
            name: tag.name.clone(),
            decls,
            types,
        }
    }
}

/// Describes the cursor at byte offset `cursor` (between `text[cursor - 1]` and
/// `text[cursor]`). Offsets past the end or inside a character are clamped.
pub fn cursor_context(text: &str, cursor: usize) -> CursorContext {
    let mut c = cursor.min(text.len());
    while !text.is_char_boundary(c) {
        c -= 1;
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut lexer = Lexer::new(text, 0);
    let mut in_tag = false;
    let content = |stack: &[Frame]| {
        if stack.is_empty() {
            CursorLocation::Outside
        } else {
            CursorLocation::Text
        }
    };
    let location = loop {
        let Some(con) = lexer.next_construct() else {
            break content(&stack);
        };
        let span = con.span();
        if c <= span.start {
            break content(&stack);
        }
        let inside = c < span.end || (c == span.end && !con.terminated());
        if !inside {
            match &con {
                Construct::StartTag(t) if !t.self_closing => {
                    stack.push(Frame::new(text, t, &lexer.attrs));
                }
                Construct::EndTag(t) => {
                    let name = &text[t.name.clone()];
                    if let Some(i) = stack.iter().rposition(|f| &text[f.name.clone()] == name) {
                        stack.truncate(i);
                    }
                }
                _ => {}
            }
            continue;
        }
        break match con {
            Construct::Text(_) => content(&stack),
            Construct::Comment { .. } => CursorLocation::Comment,
            Construct::CData { .. } => CursorLocation::CData,
            Construct::Pi { .. } | Construct::Doctype { .. } => CursorLocation::Markup,
            Construct::Stray(r) if r.len() == 1 => CursorLocation::ElementName {
                closing: false,
                name: c..c,
            },
            Construct::Stray(_) => CursorLocation::Markup,
            Construct::EndTag(t) => {
                in_tag = !stack.is_empty();
                if (t.name.start..=t.name.end).contains(&c) {
                    CursorLocation::ElementName {
                        closing: true,
                        name: t.name,
                    }
                } else {
                    CursorLocation::Tag
                }
            }
            Construct::StartTag(t) => {
                stack.push(Frame::new(text, &t, &lexer.attrs));
                in_tag = true;
                start_tag_location(text, &t, &lexer.attrs, c)
            }
        };
    };

    let path = resolve_path(text, &stack);
    let location = match location {
        CursorLocation::AttributeValue {
            attribute, value, ..
        } => {
            let name = path
                .last()
                .and_then(|e| e.namespaces.resolve_attribute(&attribute));
            CursorLocation::AttributeValue {
                attribute,
                name,
                value,
            }
        }
        other => other,
    };
    CursorContext {
        path,
        location,
        in_tag,
    }
}

fn start_tag_location(text: &str, t: &TagInfo, attrs: &[RawAttr], c: usize) -> CursorLocation {
    if (t.name.start..=t.name.end).contains(&c) {
        return CursorLocation::ElementName {
            closing: false,
            name: t.name.clone(),
        };
    }
    for a in attrs {
        if (a.name.start..=a.name.end).contains(&c) {
            return CursorLocation::AttributeName {
                name: a.name.clone(),
            };
        }
        if let (Some(v), Some(content)) = (&a.value, a.content())
            && v.start < c
            && (c < v.end || (!a.value_terminated && c == v.end))
        {
            return CursorLocation::AttributeValue {
                attribute: text[a.name.clone()].to_owned(),
                name: None,
                value: content,
            };
        }
    }
    if text.as_bytes().get(c - 1).copied().is_some_and(is_xml_ws) {
        CursorLocation::AttributeName { name: c..c }
    } else {
        CursorLocation::Tag
    }
}

fn resolve_path(text: &str, stack: &[Frame]) -> Vec<PathElement> {
    let xsi_type = QName::new(XSI_NS, "type");
    let mut path: Vec<PathElement> = Vec::with_capacity(stack.len());
    for f in stack {
        let mut namespaces = path
            .last()
            .map(|p| p.namespaces.clone())
            .unwrap_or_default();
        for (name, value) in &f.decls {
            let Some(prefix) = xmlns_prefix(&text[name.clone()]) else {
                continue;
            };
            let uri = unescape_lossy(&text[value.clone()]);
            // `xmlns:p=""` is an error in XML 1.0; ignore it rather than unbinding `p`.
            if prefix == "xml" || prefix == "xmlns" || (!prefix.is_empty() && uri.is_empty()) {
                continue;
            }
            namespaces.bind(prefix, uri);
        }
        let raw_name = text[f.name.clone()].to_owned();
        let name = namespaces.resolve_element(&raw_name);
        let xsi = f.types.iter().find_map(|(attr, value)| {
            (namespaces.resolve_attribute(&text[attr.clone()]).as_ref() == Some(&xsi_type)).then(
                || {
                    let raw = unescape_lossy(&text[value.clone()]);
                    let raw = raw.trim_matches(|c: char| c.is_ascii() && is_xml_ws(c as u8));
                    XsiType {
                        raw: raw.to_owned(),
                        name: namespaces.resolve_qname_value(raw),
                    }
                },
            )
        });
        path.push(PathElement {
            raw_name,
            name,
            start: f.start,
            namespaces,
            xsi_type: xsi,
        });
    }
    path
}
