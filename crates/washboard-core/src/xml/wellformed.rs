//! Well-formedness check for the editor (debounced on every edit) and the validation pipeline.
//!
//! `quick-xml` does the tokenizing and the checks it knows (tag matching, comment syntax,
//! attribute syntax, unclosed markup). On top of it this module checks what quick-xml leaves
//! out: one root element, no text outside it, valid names, references (predefined entities and
//! valid character references only, unless a DOCTYPE might declare more), `<` in attribute
//! values, whitespace between attributes, illegal characters, and the Namespaces in XML rules
//! (declared prefixes, reserved prefixes, duplicate attributes by expanded name).

use std::fmt::Write as _;

use quick_xml::Reader;
use quick_xml::errors::{Error as QxError, IllFormedError, SyntaxError};
use quick_xml::events::attributes::AttrError;
use quick_xml::events::{BytesStart, Event};

use crate::diag::{DiagSource, Diagnostic, pos_at_byte};

use super::names::{is_qname, is_xml_ws, split_qname, unescape_lossy, xmlns_prefix};
use super::{XML_NS, XMLNS_NS};

/// The first well-formedness error of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WellFormednessError {
    /// Byte offset the error points at, for the editor's underline. [`Self::diagnostic`] carries
    /// the same position as line/column.
    pub offset: usize,
    pub diagnostic: Diagnostic,
}

/// Checks `text` (already decoded, see [`super::decode`]) and returns the first error.
///
/// Messages start with "not well-formed:". Positions point at the start of the offending
/// construct: the `<` of a mismatched end tag, the name of a duplicate attribute, the `<` of
/// the innermost element left open at the end of input.
pub fn check_well_formed(text: &str) -> Result<(), Diagnostic> {
    match well_formedness_error(text) {
        Some(e) => Err(e.diagnostic),
        None => Ok(()),
    }
}

/// Like [`check_well_formed`], with the byte offset of the error.
pub fn well_formedness_error(text: &str) -> Option<WellFormednessError> {
    Checker::new(text).run().err()
}

const PREDEFINED: [&str; 5] = ["lt", "gt", "amp", "apos", "quot"];

#[derive(Debug, Default)]
struct AttrBuf {
    key: String,
    value: String,
    /// Offset of the attribute name.
    offset: usize,
    /// Offset just after the value's closing quote, if known.
    value_end: Option<usize>,
}

#[derive(Debug)]
struct Checker<'a> {
    text: &'a str,
    /// In-scope namespace bindings, innermost last: (prefix, uri). `""` is the default.
    ns: Vec<(String, String)>,
    /// For each open element: offset of its `<` and `ns.len()` before its declarations.
    open: Vec<(usize, usize)>,
    attrs: Vec<AttrBuf>,
    n_attrs: usize,
    doctype: bool,
    root_seen: bool,
}

type Check<T = ()> = Result<T, WellFormednessError>;

impl<'a> Checker<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            ns: Vec::new(),
            open: Vec::new(),
            attrs: Vec::new(),
            n_attrs: 0,
            doctype: false,
            root_seen: false,
        }
    }

    fn err(&self, offset: usize, msg: impl std::fmt::Display) -> WellFormednessError {
        let offset = offset.min(self.text.len());
        let pos = pos_at_byte(self.text, offset);
        WellFormednessError {
            offset,
            diagnostic: Diagnostic::error(
                DiagSource::WellFormedness,
                Some(pos),
                format!("not well-formed: {msg}"),
            ),
        }
    }

    fn run(mut self) -> Check {
        let mut reader = Reader::from_str(self.text);
        let cfg = reader.config_mut();
        cfg.check_end_names = true;
        cfg.check_comments = true;
        cfg.allow_unmatched_ends = false;
        cfg.allow_dangling_amp = false;
        cfg.expand_empty_elements = false;
        cfg.trim_text(false);
        let illegal = first_illegal_char(self.text);
        let to_usize = |p: u64| usize::try_from(p).unwrap_or(usize::MAX);
        loop {
            let before = to_usize(reader.buffer_position());
            let event = reader.read_event();
            let after = to_usize(reader.buffer_position());
            if let Some((at, ch)) = illegal
                && at < after
            {
                return Err(self.err(
                    at,
                    format_args!("character U+{:04X} is not allowed in XML", u32::from(ch)),
                ));
            }
            match event {
                Err(e) => {
                    let at = to_usize(reader.error_position());
                    return Err(self.parser_error(&e, at, before));
                }
                Ok(Event::Eof) => break,
                Ok(Event::Start(e)) => self.start(&e, before, false)?,
                Ok(Event::Empty(e)) => self.start(&e, before, true)?,
                Ok(Event::End(_)) => {
                    if let Some((_, mark)) = self.open.pop() {
                        self.ns.truncate(mark);
                    }
                }
                Ok(Event::Text(t)) => {
                    let raw: &str = &t;
                    if self.open.is_empty()
                        && let Some(i) = raw.bytes().position(|b| !is_xml_ws(b))
                    {
                        return Err(if self.root_seen {
                            self.err(before + i, "text is not allowed after the root element")
                        } else {
                            self.err(before + i, "text is not allowed before the root element")
                        });
                    }
                    if let Some(i) = raw.find("]]>") {
                        return Err(self.err(before + i, "`]]>` is not allowed in text"));
                    }
                }
                Ok(Event::GeneralRef(r)) => {
                    if self.open.is_empty() {
                        return Err(self.err(before, "references are only allowed inside elements"));
                    }
                    let name: &str = &r;
                    if let Some(msg) = self.reference_problem(name) {
                        return Err(self.err(before, msg));
                    }
                }
                Ok(Event::CData(_)) => {
                    if self.open.is_empty() {
                        return Err(self.err(before, "CDATA is only allowed inside elements"));
                    }
                }
                Ok(Event::Decl(d)) => {
                    if before != 0 {
                        return Err(self.err(
                            before,
                            "the XML declaration is only allowed at the very start of the document",
                        ));
                    }
                    if let Err(e) = d.version() {
                        return Err(self.parser_error(&e, before, before));
                    }
                }
                Ok(Event::PI(p)) => {
                    let target = p.target();
                    if !is_qname(target) || target.contains(':') {
                        return Err(self.err(
                            before,
                            "processing instruction needs a target name after `<?`",
                        ));
                    }
                    if target.eq_ignore_ascii_case("xml") {
                        return Err(self.err(before, "`xml` is reserved and not a PI target"));
                    }
                }
                Ok(Event::DocType(_)) => {
                    if self.root_seen {
                        return Err(self.err(before, "DOCTYPE must come before the root element"));
                    }
                    if self.doctype {
                        return Err(self.err(before, "only one DOCTYPE is allowed"));
                    }
                    self.doctype = true;
                }
                Ok(Event::Comment(_)) => {}
            }
        }
        if let Some(&(start, _)) = self.open.last() {
            let name = name_at(self.text, start + 1);
            return Err(self.err(start, format_args!("element `<{name}>` is not closed")));
        }
        if !self.root_seen {
            return Err(self.err(self.text.len(), "the document has no root element"));
        }
        Ok(())
    }

    fn reference_problem(&self, name: &str) -> Option<String> {
        if let Some(num) = name.strip_prefix('#') {
            let parsed = match num.strip_prefix('x') {
                Some(hex) if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    u32::from_str_radix(hex, 16).ok()
                }
                None if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) => {
                    num.parse().ok()
                }
                _ => None,
            };
            return match parsed.and_then(char::from_u32).filter(|&c| is_xml_char(c)) {
                Some(_) => None,
                None => Some(format!("`&{name};` is not a valid character reference")),
            };
        }
        if PREDEFINED.contains(&name) || self.doctype {
            None
        } else {
            Some(format!(
                "undefined entity `&{name};` (only &lt; &gt; &amp; &apos; &quot; are predefined)"
            ))
        }
    }

    fn parser_error(&self, e: &QxError, at: usize, before: usize) -> WellFormednessError {
        match e {
            QxError::IllFormed(IllFormedError::MismatchedEndTag { expected, found }) => {
                let mut msg =
                    format!("end tag `</{found}>` does not match start tag `<{expected}>`");
                if let Some(&(start, _)) = self.open.last() {
                    let line = pos_at_byte(self.text, start).line;
                    let _ = write!(msg, " (line {line})");
                }
                self.err(at, msg)
            }
            QxError::IllFormed(IllFormedError::UnmatchedEndTag(found)) => self.err(
                at,
                format_args!("end tag `</{found}>` has no matching start tag"),
            ),
            QxError::IllFormed(IllFormedError::DoubleHyphenInComment) => {
                self.err(at, "`--` is not allowed inside a comment")
            }
            QxError::IllFormed(IllFormedError::UnclosedReference) => self.err(
                at,
                "`&` starts a reference; write `&amp;` for a literal ampersand",
            ),
            QxError::IllFormed(other) => self.err(at, other),
            QxError::Syntax(s) => {
                let msg = match s {
                    SyntaxError::InvalidBangMarkup => "`<!` must start a comment, CDATA or DOCTYPE",
                    SyntaxError::UnclosedPI => "processing instruction is not closed with `?>`",
                    SyntaxError::UnclosedXmlDecl => "XML declaration is not closed with `?>`",
                    SyntaxError::UnclosedComment => "comment is not closed with `-->`",
                    SyntaxError::UnclosedDoctype => "DOCTYPE is not closed with `>`",
                    SyntaxError::UnclosedCData => "CDATA section is not closed with `]]>`",
                    SyntaxError::UnclosedTag => "tag is not closed with `>`",
                    SyntaxError::UnclosedSingleQuotedAttributeValue => {
                        "attribute value is not closed with `'`"
                    }
                    SyntaxError::UnclosedDoubleQuotedAttributeValue => {
                        "attribute value is not closed with `\"`"
                    }
                };
                self.err(at, msg)
            }
            QxError::InvalidAttr(a) => self.attr_error(a, before),
            other => self.err(at, other),
        }
    }

    /// Attribute error positions are relative to the byte after the tag's `<`.
    fn attr_error(&self, e: &AttrError, tag_start: usize) -> WellFormednessError {
        let base = tag_start + 1;
        match *e {
            AttrError::ExpectedEq(p) => {
                // The position is after the name (and any whitespace); point at the name.
                let before_ws = self
                    .text
                    .get(..base + p)
                    .unwrap_or_default()
                    .trim_end_matches(|c: char| c.is_ascii() && is_xml_ws(c as u8));
                let start = before_ws
                    .rfind(|c: char| c.is_ascii() && (is_xml_ws(c as u8) || c == '<'))
                    .map_or(0, |i| i + 1);
                let name = &before_ws[start..];
                self.err(start, format_args!("attribute `{name}` needs `=\"value\"`"))
            }
            AttrError::ExpectedValue(p) => {
                self.err(base + p, "expected an attribute value after `=`")
            }
            AttrError::UnquotedValue(p) => self.err(base + p, "attribute values must be quoted"),
            AttrError::ExpectedQuote(p, q) => self.err(
                base + p,
                format_args!("attribute value is not closed with `{}`", char::from(q)),
            ),
            AttrError::Duplicated(p, prev) => {
                let name = name_at(self.text, base + p);
                let line = pos_at_byte(self.text, base + prev).line;
                self.err(
                    base + p,
                    format_args!("duplicate attribute `{name}` (first on line {line})"),
                )
            }
        }
    }

    fn start(&mut self, e: &BytesStart<'_>, before: usize, empty: bool) -> Check {
        if self.open.is_empty() && self.root_seen {
            return Err(self.err(before, "only one root element is allowed"));
        }
        self.root_seen = true;
        let name = e.name();
        let name: &str = name.as_ref();
        if !is_qname(name) {
            return Err(self.err(
                before + 1,
                format_args!("`{name}` is not a valid element name"),
            ));
        }
        self.collect_attrs(e, before)?;
        let mark = self.ns.len();

        // Declarations first: they apply to the element's own name and attributes.
        for i in 0..self.n_attrs {
            let a = &self.attrs[i];
            let Some(prefix) = xmlns_prefix(&a.key) else {
                continue;
            };
            let uri = unescape_lossy(&a.value).into_owned();
            let problem = if prefix == "xmlns" {
                Some("the `xmlns` prefix cannot be declared".to_owned())
            } else if prefix == "xml" && uri != XML_NS {
                Some(format!("the `xml` prefix can only be bound to {XML_NS}"))
            } else if prefix != "xml" && uri == XML_NS {
                Some(format!("only the `xml` prefix can be bound to {XML_NS}"))
            } else if uri == XMLNS_NS {
                Some(format!("{XMLNS_NS} cannot be bound to a prefix"))
            } else if !prefix.is_empty() && uri.is_empty() {
                Some(format!(
                    "`xmlns:{prefix}=\"\"` cannot undeclare a prefix in XML 1.0"
                ))
            } else {
                None
            };
            if let Some(msg) = problem {
                return Err(self.err(a.offset, msg));
            }
            self.ns.push((prefix.to_owned(), uri));
        }

        if let (Some(prefix), _) = split_qname(name)
            && self.namespace(prefix).is_none()
        {
            return Err(self.err(
                before + 1,
                format_args!("namespace prefix `{prefix}` is not declared"),
            ));
        }
        for i in 0..self.n_attrs {
            let a = &self.attrs[i];
            if xmlns_prefix(&a.key).is_some() {
                continue;
            }
            let (Some(prefix), local) = split_qname(&a.key) else {
                continue;
            };
            let Some(ns) = self.namespace(prefix) else {
                return Err(self.err(
                    a.offset,
                    format_args!("namespace prefix `{prefix}` is not declared"),
                ));
            };
            for b in &self.attrs[..i] {
                if let (Some(bp), bl) = split_qname(&b.key)
                    && bl == local
                    && xmlns_prefix(&b.key).is_none()
                    && self.namespace(bp) == Some(ns)
                {
                    return Err(self.err(
                        a.offset,
                        format_args!(
                            "attributes `{}` and `{}` have the same namespace and name",
                            b.key, a.key
                        ),
                    ));
                }
            }
        }

        if empty {
            self.ns.truncate(mark);
        } else {
            self.open.push((before, mark));
        }
        Ok(())
    }

    fn namespace(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(XML_NS);
        }
        if prefix == "xmlns" {
            return None;
        }
        self.ns
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
            .filter(|u| !u.is_empty())
    }

    /// Reads the attributes into `self.attrs` (reusing their buffers) and checks their syntax.
    fn collect_attrs(&mut self, e: &BytesStart<'_>, before: usize) -> Check {
        self.n_attrs = 0;
        let text = self.text;
        for a in e.attributes() {
            let a = match a {
                Ok(a) => a,
                Err(err) => return Err(self.attr_error(&err, before)),
            };
            let key: &str = a.key.as_ref();
            let offset = offset_of(text, key).unwrap_or(before);
            let value_start = offset_of(text, &a.value);
            if !is_qname(key) {
                return Err(self.err(
                    offset,
                    format_args!("`{key}` is not a valid attribute name"),
                ));
            }
            if let Some(i) = a.value.find('<') {
                return Err(self.err(
                    value_start.map_or(offset, |v| v + i),
                    "`<` is not allowed in attribute values; write `&lt;`",
                ));
            }
            if let Some((i, msg)) = self.value_reference_problem(&a.value) {
                return Err(self.err(value_start.map_or(offset, |v| v + i), msg));
            }
            if self.attrs.len() == self.n_attrs {
                self.attrs.push(AttrBuf::default());
            }
            let slot = &mut self.attrs[self.n_attrs];
            slot.key.clear();
            slot.key.push_str(key);
            slot.value.clear();
            slot.value.push_str(&a.value);
            slot.offset = offset;
            slot.value_end = value_start.map(|v| v + a.value.len() + 1);
            self.n_attrs += 1;
        }
        // XML requires whitespace between attributes; quick-xml accepts `a='1'b='2'`.
        let tag_end = before + 1 + e.len();
        for a in &self.attrs[..self.n_attrs] {
            if let Some(end) = a.value_end
                && end < tag_end
                && let Some(&c) = text.as_bytes().get(end)
                && !is_xml_ws(c)
                && c != b'/'
            {
                return Err(self.err(end, "attributes must be separated by whitespace"));
            }
        }
        Ok(())
    }

    /// First problem with a reference in a raw attribute value: (byte index, message).
    fn value_reference_problem(&self, raw: &str) -> Option<(usize, String)> {
        let mut from = 0;
        while let Some(i) = raw[from..].find('&').map(|i| from + i) {
            let Some(len) = raw[i + 1..].find(';') else {
                return Some((
                    i,
                    "`&` starts a reference; write `&amp;` for a literal ampersand".into(),
                ));
            };
            let name = &raw[i + 1..i + 1 + len];
            if !name.starts_with('#') && !is_qname(name) {
                return Some((
                    i,
                    "`&` starts a reference; write `&amp;` for a literal ampersand".into(),
                ));
            }
            if let Some(msg) = self.reference_problem(name) {
                return Some((i, msg));
            }
            from = i + 1 + len + 1;
        }
        None
    }
}

/// Byte offset of `part` within `text`, if `part` is a subslice of it.
fn offset_of(text: &str, part: &str) -> Option<usize> {
    let start = text.as_ptr() as usize;
    let p = part.as_ptr() as usize;
    (p >= start && p + part.len() <= start + text.len()).then(|| p - start)
}

/// The name starting at `at`, for messages.
fn name_at(text: &str, at: usize) -> &str {
    let Some(rest) = text.get(at..) else {
        return "";
    };
    let end = rest
        .find(|c: char| c.is_ascii() && (is_xml_ws(c as u8) || "<>/=\"'".contains(c)))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// `Char` production of XML 1.0.
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// First char outside the XML `Char` production (control characters, U+FFFE, U+FFFF).
fn first_illegal_char(text: &str) -> Option<(usize, char)> {
    let b = text.as_bytes();
    let i = b
        .iter()
        .position(|&c| (c < 0x20 && c != b'\t' && c != b'\n' && c != b'\r') || c == 0xEF)?;
    // 0xEF leads U+F000..U+FFFF; only U+FFFE/U+FFFF are illegal. Fall back to a char scan
    // from there (rare: Private Use Area and a few CJK compatibility blocks).
    text[i..]
        .char_indices()
        .find(|&(_, c)| !is_xml_char(c))
        .map(|(j, c)| (i + j, c))
}
