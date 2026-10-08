//! Tokenizer for syntax highlighting.
//!
//! Tolerant of anything: the editor highlights while the user types. Tokens are byte ranges
//! into the text, in order, non-overlapping. Whitespace inside tags is not covered by any token
//! (draw it in the default style); text content, including whitespace between elements, is.
//!
//! Incremental use: the tokenizer has no state between top-level constructs (text, tag,
//! comment, …), so it can restart at the first token of any construct
//! ([`Token::construct_start`]). [`TokenBuffer`] does the bookkeeping for an editor.

use std::ops::Range;

use super::lex::{Construct, Lexer, RawAttr};
use super::utf16::Utf16Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// `<`, `</`, `>`, `/>`, `=`, and the `:` between prefix and local name.
    Punct,
    /// Prefix of an element name (`soapenv` in `soapenv:Body`).
    TagPrefix,
    /// Local part of an element name.
    TagName,
    /// Prefix of an attribute name (`xsi` in `xsi:type`).
    AttrPrefix,
    /// Local part of an attribute name.
    AttrName,
    /// The name of a namespace declaration attribute: `xmlns` or `xmlns:p`, as one token.
    NamespaceDecl,
    /// Attribute value including quotes, minus any entity or character references inside.
    AttrValue,
    /// Character data, including whitespace between elements.
    Text,
    /// `<!-- … -->`, including the delimiters.
    Comment,
    /// `<![CDATA[ … ]]>`, including the delimiters.
    CData,
    /// `<?target … ?>` other than the XML declaration.
    ProcessingInstruction,
    /// `<?xml … ?>`.
    XmlDecl,
    /// `<!DOCTYPE … >` (not allowed in SOAP messages, but highlighted all the same).
    Doctype,
    /// `&name;`
    EntityRef,
    /// `&#123;` / `&#x1F;`
    CharRef,
    /// Something that cannot be markup: a stray `<` or `&`, junk inside a tag.
    Error,
}

/// One highlighted span. `start..end` is a byte range into the tokenized text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    /// First token of a top-level construct. Tokenizing may restart at such a token's `start`
    /// with the same result, which is what makes re-tokenizing an edited range possible.
    pub construct_start: bool,
}

impl Token {
    pub fn span(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Tokenizes the whole text.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::with_capacity(text.len() / 6);
    let mut lexer = Lexer::new(text, 0);
    while let Some(c) = lexer.next_construct() {
        emit_construct(text, &lexer, &c, &mut out);
    }
    out
}

/// Result of [`tokenize_range`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeTokens {
    pub tokens: Vec<Token>,
    /// Where tokenizing stopped: the start of the first construct at or after `min_end`, or the
    /// end of the text. Tokens from here on are the same as a full tokenization would produce
    /// *if* the previous token list also had a construct boundary here; otherwise the caller
    /// must continue from this point. [`TokenBuffer::edit`] does exactly that.
    pub end: usize,
}

/// Tokenizes from `start` until the first construct boundary at or after `min_end`.
///
/// `start` must be a construct boundary — 0 or the start of a token with `construct_start` —
/// in a part of the text that has not changed; otherwise the tokens are plausible but may
/// differ from a full pass. Out-of-range or non-char-boundary positions are clamped.
pub fn tokenize_range(text: &str, start: usize, min_end: usize) -> RangeTokens {
    let mut tokens = Vec::new();
    let mut lexer = Lexer::new(text, start);
    while lexer.pos() < min_end {
        let Some(c) = lexer.next_construct() else {
            break;
        };
        emit_construct(text, &lexer, &c, &mut tokens);
    }
    RangeTokens {
        tokens,
        end: lexer.pos(),
    }
}

/// Token list for one editor document, kept up to date across edits.
///
/// After [`TokenBuffer::edit`] the tokens equal those of a full [`tokenize`] of the new text;
/// only the returned range was re-tokenized, so the editor re-applies colours just there.
#[derive(Debug, Clone, Default)]
pub struct TokenBuffer {
    tokens: Vec<Token>,
    text_len: usize,
}

impl TokenBuffer {
    pub fn new(text: &str) -> Self {
        Self {
            tokens: tokenize(text),
            text_len: text.len(),
        }
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Tokens overlapping the byte range `range` (e.g. the visible or the re-tokenized part).
    pub fn tokens_in(&self, range: Range<usize>) -> &[Token] {
        let from = self.tokens.partition_point(|t| t.end <= range.start);
        let to = self.tokens.partition_point(|t| t.start < range.end);
        self.tokens.get(from..to.max(from)).unwrap_or(&[])
    }

    /// [`tokens_in`](Self::tokens_in) with spans as UTF-16 offsets, which AppKit's text system
    /// counts in. `text` is the text the buffer currently describes.
    pub fn tokens_utf16(&self, text: &str, range: Range<usize>) -> Vec<(Range<usize>, TokenKind)> {
        let mut cursor = Utf16Cursor::new(text);
        self.tokens_in(range)
            .iter()
            .map(|t| (cursor.utf16_range(t.span()), t.kind))
            .collect()
    }

    /// Updates the tokens after `old_range` (byte range in the previous text) was replaced by
    /// `new_len` bytes, giving `new_text`. Returns the byte range of `new_text` whose tokens
    /// changed; colours outside it stay valid (positions after it shift by the length change).
    ///
    /// Inconsistent arguments (a range outside the old text, a length that does not add up)
    /// fall back to a full re-tokenization and return the whole text.
    pub fn edit(
        &mut self,
        new_text: &str,
        old_range: Range<usize>,
        new_len: usize,
    ) -> Range<usize> {
        let consistent = old_range.start <= old_range.end
            && old_range.end <= self.text_len
            && self.text_len - (old_range.end - old_range.start) + new_len == new_text.len()
            && new_text.is_char_boundary(old_range.start)
            && new_text.is_char_boundary(old_range.start + new_len);
        if !consistent {
            *self = Self::new(new_text);
            return 0..new_text.len();
        }
        let es = old_range.start;
        let old_end = old_range.end;
        let new_end = es + new_len;

        // Restart at the last construct boundary strictly before the edit: the construct
        // ending at the edit may need to absorb the inserted text (e.g. typing `>` after `<a`).
        // Constructs look at most one char past their end, except a stray `<!`, which looks
        // further (`<!--`, `<![CDATA[`, `<!DOCTYPE`); step back over those too.
        let before = self.tokens.partition_point(|t| t.start < es);
        let restart_idx = self.tokens[..before]
            .iter()
            .rposition(|t| t.construct_start)
            .map(|mut k| {
                // A stray `<!` is always a single construct-start token.
                while k > 0 && is_stray_bang(new_text, &self.tokens[k - 1]) {
                    k -= 1;
                }
                k
            });
        let (first, restart) = match restart_idx {
            Some(k) => (k, self.tokens[k].start),
            None => (0, 0),
        };

        let mut lexer = Lexer::new(new_text, restart);
        let mut fresh = Vec::new();
        let mut old_idx = self.tokens.partition_point(|t| t.start < old_end);
        let resync_old_idx = loop {
            let p = lexer.pos();
            if p >= new_end {
                if p >= new_text.len() {
                    break self.tokens.len();
                }
                let p_old = p - new_end + old_end;
                while old_idx < self.tokens.len() && self.tokens[old_idx].start < p_old {
                    old_idx += 1;
                }
                if let Some(t) = self.tokens.get(old_idx)
                    && t.start == p_old
                    && t.construct_start
                {
                    break old_idx;
                }
            }
            match lexer.next_construct() {
                Some(c) => emit_construct(new_text, &lexer, &c, &mut fresh),
                None => break self.tokens.len(),
            }
        };
        let changed = restart..lexer.pos();

        let fresh_len = fresh.len();
        self.tokens.splice(first..resync_old_idx, fresh);
        // Kept tokens start at or after `old_end`, so the shift cannot underflow.
        for t in &mut self.tokens[first + fresh_len..] {
            t.start = t.start - old_end + new_end;
            t.end = t.end - old_end + new_end;
        }
        self.text_len = new_text.len();
        changed
    }
}

/// `text` is the new text; only called for tokens before the edit, where it is unchanged.
fn is_stray_bang(text: &str, t: &Token) -> bool {
    t.construct_start
        && t.kind == TokenKind::Error
        && text
            .get(t.start..t.end)
            .is_some_and(|s| s.starts_with("<!"))
}

fn push(out: &mut Vec<Token>, kind: TokenKind, r: Range<usize>, first: &mut bool) {
    if r.is_empty() {
        return;
    }
    out.push(Token {
        kind,
        start: r.start,
        end: r.end,
        construct_start: std::mem::take(first),
    });
}

fn emit_construct(text: &str, lexer: &Lexer<'_>, c: &Construct, out: &mut Vec<Token>) {
    let mut first = true;
    let f = &mut first;
    match c {
        Construct::Text(r) => emit_with_refs(text, r.clone(), TokenKind::Text, out, f),
        Construct::Comment { span, .. } => push(out, TokenKind::Comment, span.clone(), f),
        Construct::CData { span, .. } => push(out, TokenKind::CData, span.clone(), f),
        Construct::Doctype { span, .. } => push(out, TokenKind::Doctype, span.clone(), f),
        Construct::Pi { span, target, .. } => {
            let kind = if &text[target.clone()] == "xml" {
                TokenKind::XmlDecl
            } else {
                TokenKind::ProcessingInstruction
            };
            push(out, kind, span.clone(), f);
        }
        Construct::Stray(r) => push(out, TokenKind::Error, r.clone(), f),
        Construct::EndTag(t) => {
            push(out, TokenKind::Punct, t.span.start..t.span.start + 2, f);
            emit_name(
                text,
                t.name.clone(),
                TokenKind::TagPrefix,
                TokenKind::TagName,
                out,
                f,
            );
            for j in &lexer.junk {
                push(out, TokenKind::Error, j.clone(), f);
            }
            if t.terminated {
                push(out, TokenKind::Punct, t.span.end - 1..t.span.end, f);
            }
        }
        Construct::StartTag(t) => {
            push(out, TokenKind::Punct, t.span.start..t.span.start + 1, f);
            emit_name(
                text,
                t.name.clone(),
                TokenKind::TagPrefix,
                TokenKind::TagName,
                out,
                f,
            );
            let mut junk = lexer.junk.iter().peekable();
            for a in &lexer.attrs {
                while let Some(j) = junk.next_if(|j| j.start < a.name.start) {
                    push(out, TokenKind::Error, j.clone(), f);
                }
                emit_attr(text, a, out, f);
            }
            for j in junk {
                push(out, TokenKind::Error, j.clone(), f);
            }
            if t.terminated {
                let close = if t.self_closing { 2 } else { 1 };
                push(out, TokenKind::Punct, t.span.end - close..t.span.end, f);
            }
        }
    }
}

fn emit_name(
    text: &str,
    r: Range<usize>,
    prefix_kind: TokenKind,
    local_kind: TokenKind,
    out: &mut Vec<Token>,
    f: &mut bool,
) {
    match text[r.clone()].find(':') {
        Some(i) => {
            let colon = r.start + i;
            push(out, prefix_kind, r.start..colon, f);
            push(out, TokenKind::Punct, colon..colon + 1, f);
            push(out, local_kind, colon + 1..r.end, f);
        }
        None => push(out, local_kind, r, f),
    }
}

fn emit_attr(text: &str, a: &RawAttr, out: &mut Vec<Token>, f: &mut bool) {
    let name = &text[a.name.clone()];
    if name == "xmlns" || name.starts_with("xmlns:") {
        push(out, TokenKind::NamespaceDecl, a.name.clone(), f);
    } else {
        emit_name(
            text,
            a.name.clone(),
            TokenKind::AttrPrefix,
            TokenKind::AttrName,
            out,
            f,
        );
    }
    if let Some(eq) = a.eq {
        push(out, TokenKind::Punct, eq..eq + 1, f);
    }
    if let Some(v) = &a.value {
        emit_with_refs(text, v.clone(), TokenKind::AttrValue, out, f);
    }
}

/// Emits `r` as `base` tokens, split around entity and character references.
fn emit_with_refs(
    text: &str,
    r: Range<usize>,
    base: TokenKind,
    out: &mut Vec<Token>,
    f: &mut bool,
) {
    let b = text.as_bytes();
    let mut plain = r.start;
    let mut i = r.start;
    while let Some(off) = b[i..r.end].iter().position(|&c| c == b'&') {
        let amp = i + off;
        push(out, base, plain..amp, f);
        let (kind, end) = reference_at(b, amp, r.end);
        push(out, kind, amp..end, f);
        plain = end;
        i = end;
    }
    push(out, base, plain..r.end, f);
}

/// Classifies the reference starting with `&` at `amp`; a malformed one is a 1-byte error.
fn reference_at(b: &[u8], amp: usize, limit: usize) -> (TokenKind, usize) {
    let body_start = amp + 1;
    let (kind, digits_start, ok): (TokenKind, usize, fn(u8) -> bool) =
        match (b.get(body_start), b.get(body_start + 1)) {
            (Some(b'#'), Some(b'x')) => (TokenKind::CharRef, body_start + 2, |c| {
                c.is_ascii_hexdigit()
            }),
            (Some(b'#'), _) => (TokenKind::CharRef, body_start + 1, |c| c.is_ascii_digit()),
            _ => (TokenKind::EntityRef, body_start, |c| {
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':') || c >= 0x80
            }),
        };
    let mut j = digits_start;
    while j < limit && ok(b[j]) {
        j += 1;
    }
    if j > digits_start && j < limit && b[j] == b';' {
        (kind, j + 1)
    } else {
        (TokenKind::Error, body_start)
    }
}
