//! Start tags as byte ranges, for code that needs positions `roxmltree` does not expose: where
//! an element's name ends (to insert namespace declarations), which attributes are written on
//! the tag, and where the tag begins and ends (to map libxml2's line numbers to start tags).
//!
//! Built on the highlighting tokenizer, so there is one XML lexer in the code base.

use std::ops::Range;

use super::tokens::{Token, TokenKind, tokenize, tokenize_range};

/// One start tag or empty-element tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartTag {
    /// Byte offset of the `<`.
    pub start: usize,
    /// The element name as written, including any prefix.
    pub name: Range<usize>,
    /// Attribute names as written, including `xmlns` and `xmlns:p` declarations.
    pub attr_names: Vec<Range<usize>>,
    /// The value of each attribute in `attr_names` (same index), including its quotes;
    /// `None` when no value was written.
    pub attr_values: Vec<Option<Range<usize>>>,
    /// Byte offset just past the closing `>` or `/>`.
    pub end: usize,
}

impl StartTag {
    /// The local part of the element name.
    pub fn local<'a>(&self, text: &'a str) -> &'a str {
        let name = &text[self.name.clone()];
        name.rsplit(':').next().unwrap_or(name)
    }
}

/// All complete start tags in document order. Comments, CDATA, processing instructions and
/// the DOCTYPE are skipped; an unterminated tag at the end of a broken document is left out.
pub fn start_tags(text: &str) -> Vec<StartTag> {
    collect(text, &tokenize(text))
}

/// The start tag whose `<` is at byte `start`, if there is one and it is complete.
pub fn start_tag_at(text: &str, start: usize) -> Option<StartTag> {
    if text.as_bytes().get(start) != Some(&b'<') {
        return None;
    }
    let tokens = tokenize_range(text, start, start + 1).tokens;
    collect(text, &tokens)
        .into_iter()
        .next()
        .filter(|t| t.start == start)
}

fn collect(text: &str, tokens: &[Token]) -> Vec<StartTag> {
    let mut out = Vec::new();
    let mut tag: Option<StartTag> = None;
    let mut attr_start: Option<usize> = None;
    for t in tokens {
        match t.kind {
            TokenKind::Punct => match &text[t.span()] {
                "<" => {
                    tag = Some(StartTag {
                        start: t.start,
                        name: t.end..t.end,
                        attr_names: Vec::new(),
                        attr_values: Vec::new(),
                        end: t.end,
                    });
                }
                ">" | "/>" => {
                    if let Some(mut done) = tag.take().filter(|d| !d.name.is_empty()) {
                        done.end = t.end;
                        out.push(done);
                    }
                    attr_start = None;
                }
                "</" => tag = None,
                _ => {}
            },
            TokenKind::TagPrefix | TokenKind::TagName => {
                if let Some(tag) = &mut tag {
                    if tag.name.is_empty() {
                        tag.name.start = t.start;
                    }
                    tag.name.end = t.end;
                }
            }
            TokenKind::AttrPrefix => attr_start = Some(t.start),
            TokenKind::AttrName | TokenKind::NamespaceDecl => {
                if let Some(tag) = &mut tag {
                    tag.attr_names
                        .push(attr_start.take().unwrap_or(t.start)..t.end);
                    tag.attr_values.push(None);
                }
            }
            // A value with references is several tokens; together they cover the value.
            TokenKind::AttrValue | TokenKind::EntityRef | TokenKind::CharRef => {
                if let Some(value) = tag.as_mut().and_then(|t| t.attr_values.last_mut()) {
                    let start = value.as_ref().map_or(t.start, |v| v.start);
                    *value = Some(start..t.end);
                }
            }
            _ if t.construct_start => {
                // Any other construct ends whatever tag was being read (broken markup).
                tag = None;
                attr_start = None;
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<'a>(text: &'a str, tags: &[StartTag]) -> Vec<(&'a str, Vec<&'a str>)> {
        tags.iter()
            .map(|t| {
                let attrs = t.attr_names.iter().map(|r| &text[r.clone()]).collect();
                (&text[t.name.clone()], attrs)
            })
            .collect()
    }

    #[test]
    fn finds_start_tags_and_attribute_names() {
        let text = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n<!ENTITY x \"<y>\">\n<!-- ] > -->\n]>\n\
                    <r a='>' xmlns:p='u'><!-- <c> --><![CDATA[<d>]]><?pi <e>?>\n<p:f\n xsi:type=\"t\"/></r>";
        let tags = start_tags(text);
        assert_eq!(
            names(text, &tags),
            [("r", vec!["a", "xmlns:p"]), ("p:f", vec!["xsi:type"]),]
        );
        assert_eq!(tags[1].local(text), "f");
        assert_eq!(&text[tags[1].start..tags[1].end], "<p:f\n xsi:type=\"t\"/>");
    }

    #[test]
    fn start_tag_at_a_position() {
        let text = "<a><b x='1'>t</b></a>";
        let b = start_tag_at(text, 3).expect("tag");
        assert_eq!(&text[b.name.clone()], "b");
        assert_eq!(b.end, 12);
        assert_eq!(start_tag_at(text, 13), None); // `</b>`
        assert_eq!(start_tag_at(text, 1), None); // not a `<`
    }

    #[test]
    fn attribute_spacing_and_markup_in_values() {
        let t = "x <xs:schema xmlns:xs='u' a = \"v>w\"\n targetNamespace=\"t\"><a/>";
        let st = start_tag_at(t, 2).expect("start tag");
        assert_eq!(&t[2..st.name.end], "<xs:schema");
        let attrs: Vec<&str> = st.attr_names.iter().map(|r| &t[r.clone()]).collect();
        assert_eq!(attrs, ["xmlns:xs", "a", "targetNamespace"]);
        let values: Vec<Option<&str>> = st
            .attr_values
            .iter()
            .map(|v| v.clone().map(|r| &t[r]))
            .collect();
        assert_eq!(values, [Some("'u'"), Some("\"v>w\""), Some("\"t\"")]);
        let refs = "<a b='x&amp;y&#65;z'/>";
        let st = start_tag_at(refs, 0).expect("start tag");
        assert_eq!(st.attr_values, [Some(5..20)]);
        assert!(start_tag_at(t, 0).is_none());
        assert!(start_tag_at("<a b='x", 0).is_none());
        assert_eq!(start_tag_at("<a/>", 0).expect("empty").name.end, 2);
    }

    #[test]
    fn unterminated_tag_is_left_out() {
        assert!(start_tags("<a><b x='1'").iter().all(|t| t.start != 3));
    }
}
