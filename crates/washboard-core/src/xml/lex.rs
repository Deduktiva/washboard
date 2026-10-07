//! Tolerant markup lexer shared by the tokenizer, cursor context and pretty-printer.
//!
//! Splits text into top-level constructs (text, tags, comments, …). It never fails: broken
//! markup ends at the next `<` or at the end of input, so one typo does not swallow the rest of
//! the document (except for comments, CDATA, PIs and DOCTYPE, which by definition run to their
//! terminator). Its state between constructs is always "in content", independent of element
//! nesting; incremental re-tokenization relies on that.
//!
//! Every range ends on an ASCII byte or at the end of input, so all ranges are valid `&str`
//! slice bounds.

use std::ops::Range;

use super::names::is_xml_ws;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TagInfo {
    /// From `<` to after `>` (or to where the broken tag was cut off).
    pub span: Range<usize>,
    /// Element name as written; may be empty for `</>` or `</` at the end of input.
    pub name: Range<usize>,
    pub self_closing: bool,
    /// Whether the tag ended with `>` / `/>`.
    pub terminated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawAttr {
    pub name: Range<usize>,
    pub eq: Option<usize>,
    /// Including the quotes (the closing quote only if `value_terminated`).
    pub value: Option<Range<usize>>,
    pub value_terminated: bool,
}

impl RawAttr {
    /// The value without quotes.
    pub fn content(&self) -> Option<Range<usize>> {
        let v = self.value.as_ref()?;
        let end = if self.value_terminated {
            v.end - 1
        } else {
            v.end
        };
        Some(v.start + 1..end.max(v.start + 1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Construct {
    Text(Range<usize>),
    /// Attributes and junk are in [`Lexer::attrs`] / [`Lexer::junk`] until the next call.
    StartTag(TagInfo),
    EndTag(TagInfo),
    Comment {
        span: Range<usize>,
        terminated: bool,
    },
    CData {
        span: Range<usize>,
        terminated: bool,
    },
    Pi {
        span: Range<usize>,
        target: Range<usize>,
        terminated: bool,
    },
    Doctype {
        span: Range<usize>,
        terminated: bool,
    },
    /// `<` or `<!` that does not start any markup.
    Stray(Range<usize>),
}

impl Construct {
    pub fn span(&self) -> Range<usize> {
        match self {
            Construct::Text(s) | Construct::Stray(s) => s.clone(),
            Construct::StartTag(t) | Construct::EndTag(t) => t.span.clone(),
            Construct::Comment { span, .. }
            | Construct::CData { span, .. }
            | Construct::Pi { span, .. }
            | Construct::Doctype { span, .. } => span.clone(),
        }
    }

    pub fn terminated(&self) -> bool {
        match self {
            Construct::Text(_) => true,
            // A stray `<` is markup the user has not finished typing.
            Construct::Stray(_) => false,
            Construct::StartTag(t) | Construct::EndTag(t) => t.terminated,
            Construct::Comment { terminated, .. }
            | Construct::CData { terminated, .. }
            | Construct::Pi { terminated, .. }
            | Construct::Doctype { terminated, .. } => *terminated,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Lexer<'a> {
    text: &'a str,
    b: &'a [u8],
    pos: usize,
    /// Attributes of the last start tag (or empty).
    pub attrs: Vec<RawAttr>,
    /// Characters in the last tag that fit no attribute syntax, in source order.
    pub junk: Vec<Range<usize>>,
}

/// Bytes that end a name inside markup. Everything else (including non-ASCII) is a name byte;
/// the lexer is deliberately liberal and leaves name validity to the well-formedness check.
fn is_name_byte(b: u8) -> bool {
    !is_xml_ws(b) && !matches!(b, b'<' | b'>' | b'/' | b'=' | b'"' | b'\'')
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == ':' || !c.is_ascii()
}

impl<'a> Lexer<'a> {
    /// Starts lexing at `pos`, which must be a construct boundary for meaningful results.
    /// Positions that are not char boundaries are moved back to one.
    pub fn new(text: &'a str, pos: usize) -> Self {
        let mut pos = pos.min(text.len());
        while !text.is_char_boundary(pos) {
            pos -= 1;
        }
        Self {
            text,
            b: text.as_bytes(),
            pos,
            attrs: Vec::new(),
            junk: Vec::new(),
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    fn find_from(&self, from: usize, pat: &str) -> Option<usize> {
        self.text.get(from..)?.find(pat).map(|i| from + i)
    }

    fn skip_ws(&mut self) {
        while self.pos < self.b.len() && is_xml_ws(self.b[self.pos]) {
            self.pos += 1;
        }
    }

    fn scan_name(&mut self) -> Range<usize> {
        let start = self.pos;
        while self.pos < self.b.len() && is_name_byte(self.b[self.pos]) {
            self.pos += 1;
        }
        start..self.pos
    }

    /// Advances past one char inside a tag and records it as junk.
    fn junk_char(&mut self) {
        let start = self.pos;
        let len = self.text[start..].chars().next().map_or(1, char::len_utf8);
        self.pos += len;
        match self.junk.last_mut() {
            Some(last) if last.end == start => last.end = self.pos,
            _ => self.junk.push(start..self.pos),
        }
    }

    pub fn next_construct(&mut self) -> Option<Construct> {
        self.attrs.clear();
        self.junk.clear();
        let start = self.pos;
        let rest = self.b.get(start..).filter(|r| !r.is_empty())?;
        if rest[0] != b'<' {
            let end = rest
                .iter()
                .position(|&c| c == b'<')
                .map_or(self.b.len(), |i| start + i);
            self.pos = end;
            return Some(Construct::Text(start..end));
        }
        let tail = &self.text[start..];
        if tail.starts_with("<!--") {
            let (end, terminated) = self.until(start + 4, "-->");
            return Some(Construct::Comment {
                span: start..end,
                terminated,
            });
        }
        if tail.starts_with("<![CDATA[") {
            let (end, terminated) = self.until(start + 9, "]]>");
            return Some(Construct::CData {
                span: start..end,
                terminated,
            });
        }
        if tail.starts_with("<!DOCTYPE") {
            return Some(self.doctype(start));
        }
        if tail.starts_with("<?") {
            self.pos = start + 2;
            let target = self.scan_name();
            let (end, terminated) = self.until(self.pos, "?>");
            return Some(Construct::Pi {
                span: start..end,
                target,
                terminated,
            });
        }
        if tail.starts_with("</") {
            self.pos = start + 2;
            let name = self.scan_name();
            let terminated = self.tag_rest();
            return Some(Construct::EndTag(TagInfo {
                span: start..self.pos,
                name,
                self_closing: false,
                terminated,
            }));
        }
        if tail.starts_with("<!") {
            self.pos = start + 2;
            return Some(Construct::Stray(start..self.pos));
        }
        if !tail[1..].chars().next().is_some_and(is_name_start) {
            self.pos = start + 1;
            return Some(Construct::Stray(start..self.pos));
        }
        self.pos = start + 1;
        let name = self.scan_name();
        let mut self_closing = false;
        let terminated = loop {
            self.skip_ws();
            let Some(&c) = self.b.get(self.pos) else {
                break false;
            };
            match c {
                b'>' => {
                    self.pos += 1;
                    break true;
                }
                b'/' if self.b.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    self_closing = true;
                    break true;
                }
                b'<' => break false,
                c if is_name_byte(c) => self.attribute(),
                _ => self.junk_char(),
            }
        };
        Some(Construct::StartTag(TagInfo {
            span: start..self.pos,
            name,
            self_closing,
            terminated,
        }))
    }

    /// Scans to after `pat`, or to the end of input. Returns the end and whether `pat` was found.
    fn until(&mut self, from: usize, pat: &str) -> (usize, bool) {
        let (end, found) = match self.find_from(from.min(self.b.len()), pat) {
            Some(i) => (i + pat.len(), true),
            None => (self.b.len(), false),
        };
        self.pos = end;
        (end, found)
    }

    fn doctype(&mut self, start: usize) -> Construct {
        let mut i = start + 9;
        let mut depth = 0usize;
        let mut quote: Option<u8> = None;
        while i < self.b.len() {
            let c = self.b[i];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None => match c {
                    b'"' | b'\'' => quote = Some(c),
                    b'[' => depth += 1,
                    b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => {
                        self.pos = i + 1;
                        return Construct::Doctype {
                            span: start..self.pos,
                            terminated: true,
                        };
                    }
                    _ => {}
                },
            }
            i += 1;
        }
        self.pos = self.b.len();
        Construct::Doctype {
            span: start..self.pos,
            terminated: false,
        }
    }

    /// Rest of an end tag after the name: whitespace, junk, `>`. Stops before `<`.
    fn tag_rest(&mut self) -> bool {
        loop {
            self.skip_ws();
            match self.b.get(self.pos) {
                None | Some(b'<') => return false,
                Some(b'>') => {
                    self.pos += 1;
                    return true;
                }
                Some(_) => self.junk_char(),
            }
        }
    }

    fn attribute(&mut self) {
        let name = self.scan_name();
        let after_name = self.pos;
        self.skip_ws();
        if self.b.get(self.pos) != Some(&b'=') {
            // No value; whitespace after the name belongs to the gap before what follows.
            self.pos = after_name;
            self.attrs.push(RawAttr {
                name,
                eq: None,
                value: None,
                value_terminated: false,
            });
            return;
        }
        let eq = self.pos;
        self.pos += 1;
        self.skip_ws();
        let (value, value_terminated) = match self.b.get(self.pos) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let vstart = self.pos;
                let mut i = vstart + 1;
                // `<` cannot occur in a value; stopping there keeps a missing quote from
                // swallowing the following tags.
                while i < self.b.len() && self.b[i] != q && self.b[i] != b'<' {
                    i += 1;
                }
                let terminated = i < self.b.len() && self.b[i] == q;
                self.pos = if terminated { i + 1 } else { i };
                (Some(vstart..self.pos), terminated)
            }
            _ => (None, false),
        };
        self.attrs.push(RawAttr {
            name,
            eq: Some(eq),
            value,
            value_terminated,
        });
    }
}

impl Iterator for Lexer<'_> {
    type Item = Construct;

    fn next(&mut self) -> Option<Construct> {
        self.next_construct()
    }
}
