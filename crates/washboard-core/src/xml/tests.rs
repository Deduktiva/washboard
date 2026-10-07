//! Tests for the editor-side XML utilities, mostly against `fixtures/`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::diag::{DiagSource, TextPos};
use crate::model::QName;
use crate::soap::{SOAP11_ENV_NS, XSI_NS};
use crate::test_support::{fixtures, read_fixture};

/// Every XML fixture (requests, WSDLs, XSDs), decoded.
fn all_fixtures() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![fixtures()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("fixture dir readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e == "xml" || e == "wsdl" || e == "xsd")
            {
                let bytes = std::fs::read(&path).expect("fixture readable");
                out.push((path, decode(&bytes).expect("fixture decodes").text));
            }
        }
    }
    out.sort();
    assert!(out.len() >= 15, "found {} fixtures", out.len());
    out
}

/// A char boundary in `s`, uniformly-ish. Tests seed `fastrand` so failures reproduce.
fn boundary(rng: &mut fastrand::Rng, s: &str) -> usize {
    let mut i = rng.usize(..=s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Snippets that change the tokenizer state when typed in the middle of a document.
const SNIPPETS: &[&str] = &[
    "<",
    ">",
    "/>",
    "</",
    "<!--",
    "-->",
    "-",
    "<!",
    "<![CDATA[",
    "]]>",
    "<?",
    "?>",
    "\"",
    "'",
    "=",
    "&",
    "&amp;",
    "&#x41;",
    ";",
    " ",
    "\n",
    "a",
    "x:y",
    "xmlns:p=\"u\"",
    "<!DOCTYPE",
    "[",
    "]",
    "ü",
    "😀",
    "<a>",
    "</a>",
    "<b x='1'/>",
    "\r\n",
];

fn kinds(text: &str) -> Vec<(TokenKind, &str)> {
    tokenize(text)
        .iter()
        .map(|t| (t.kind, &text[t.span()]))
        .collect()
}

/// Tokens are in order, non-overlapping, non-empty, on char boundaries; gaps are whitespace.
fn assert_token_invariants(text: &str, tokens: &[Token]) {
    let mut prev = 0;
    for t in tokens {
        assert!(t.start < t.end, "empty token {t:?} in {text:?}");
        assert!(t.start >= prev, "overlap at {t:?} in {text:?}");
        assert!(text.is_char_boundary(t.start) && text.is_char_boundary(t.end));
        assert!(
            text[prev..t.start].bytes().all(|b| b" \t\r\n".contains(&b)),
            "non-whitespace gap {:?} before {t:?} in {text:?}",
            &text[prev..t.start]
        );
        prev = t.end;
    }
    assert!(text[prev..].bytes().all(|b| b" \t\r\n".contains(&b)));
}

// ---------------------------------------------------------------- tokenizer

#[test]
fn tokenizes_all_kinds() {
    use TokenKind::*;
    let text = "<?xml version=\"1.0\"?><a:b xmlns:a=\"u\" xsi:t='1&amp;2'><!--c-->\
                <![CDATA[<x>]]>t&lt;&#x41;&bogus <?pi d?><c/></a:b>";
    assert_eq!(
        kinds(text),
        vec![
            (XmlDecl, "<?xml version=\"1.0\"?>"),
            (Punct, "<"),
            (TagPrefix, "a"),
            (Punct, ":"),
            (TagName, "b"),
            (NamespaceDecl, "xmlns:a"),
            (Punct, "="),
            (AttrValue, "\"u\""),
            (AttrPrefix, "xsi"),
            (Punct, ":"),
            (AttrName, "t"),
            (Punct, "="),
            (AttrValue, "'1"),
            (EntityRef, "&amp;"),
            (AttrValue, "2'"),
            (Punct, ">"),
            (Comment, "<!--c-->"),
            (CData, "<![CDATA[<x>]]>"),
            (Text, "t"),
            (EntityRef, "&lt;"),
            (CharRef, "&#x41;"),
            (Error, "&"),
            (Text, "bogus "),
            (ProcessingInstruction, "<?pi d?>"),
            (Punct, "<"),
            (TagName, "c"),
            (Punct, "/>"),
            (Punct, "</"),
            (TagPrefix, "a"),
            (Punct, ":"),
            (TagName, "b"),
            (Punct, ">"),
        ]
    );
}

#[test]
fn tolerates_broken_markup() {
    use TokenKind::*;
    // A tag cut off by the next `<`, junk in a tag, a stray `<`, an unterminated value.
    assert_eq!(
        kinds("<a x=\"1\" \"<b>< c<d y=\"open\n</d>"),
        vec![
            (Punct, "<"),
            (TagName, "a"),
            (AttrName, "x"),
            (Punct, "="),
            (AttrValue, "\"1\""),
            (Error, "\""),
            (Punct, "<"),
            (TagName, "b"),
            (Punct, ">"),
            (Error, "<"),
            (Text, " c"),
            (Punct, "<"),
            (TagName, "d"),
            (AttrName, "y"),
            (Punct, "="),
            (AttrValue, "\"open\n"),
            (Punct, "</"),
            (TagName, "d"),
            (Punct, ">"),
        ]
    );
    // Unterminated comment runs to the end, like in every editor.
    assert_eq!(
        kinds("<a><!-- x <b/>"),
        vec![
            (Punct, "<"),
            (TagName, "a"),
            (Punct, ">"),
            (Comment, "<!-- x <b/>"),
        ]
    );
}

#[test]
fn construct_starts_allow_restart() {
    for (path, text) in all_fixtures() {
        let full = tokenize(&text);
        assert_token_invariants(&text, &full);
        for (i, t) in full.iter().enumerate().filter(|(_, t)| t.construct_start) {
            let part = tokenize_range(&text, t.start, text.len());
            assert_eq!(part.tokens, full[i..], "restart at {} in {path:?}", t.start);
            assert_eq!(part.end, text.len());
        }
        // A bounded range stops at the first construct boundary at or after `min_end`.
        let part = tokenize_range(&text, 0, text.len() / 2);
        assert!(part.end >= text.len() / 2);
        assert_eq!(part.tokens, full[..part.tokens.len()]);
        assert!(
            full.get(part.tokens.len())
                .is_none_or(|t| t.construct_start && t.start == part.end)
        );
    }
}

#[test]
fn token_buffer_edits_match_full_tokenization() {
    let mut rng = fastrand::Rng::with_seed(0x9E37_79B9_7F4A_7C15);
    for (path, original) in all_fixtures() {
        let mut text = original.clone();
        let mut buf = TokenBuffer::new(&text);
        for step in 0..150 {
            let start = boundary(&mut rng, &text);
            let mut end = (start + rng.usize(..12)).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            if rng.usize(..3) == 0 {
                end = start; // pure insertion
            }
            let insert = if rng.usize(..4) == 0 {
                ""
            } else {
                SNIPPETS[rng.usize(..SNIPPETS.len())]
            };
            let mut new_text = String::with_capacity(text.len() + insert.len());
            new_text.push_str(&text[..start]);
            new_text.push_str(insert);
            new_text.push_str(&text[end..]);
            let changed = buf.edit(&new_text, start..end, insert.len());
            let full = tokenize(&new_text);
            assert_eq!(
                buf.tokens(),
                full.as_slice(),
                "{path:?} step {step}: replaced {start}..{end} with {insert:?}"
            );
            assert!(changed.start <= start && changed.end >= start + insert.len());
            assert!(changed.end <= new_text.len());
            text = new_text;
        }
    }
}

#[test]
fn token_buffer_typing_comment_open_rehighlights_to_its_end() {
    let text = "<a>\n  <b x=\"-->\"/>\n  <c/>\n</a>";
    let mut buf = TokenBuffer::new(text);
    let new_text = format!("<a><!--{}", &text[3..]);
    let changed = buf.edit(&new_text, 3..3, 4);
    assert_eq!(buf.tokens(), tokenize(&new_text).as_slice());
    // The new comment ends inside the attribute value; everything up to there is re-tokenized.
    assert!(changed.end >= new_text.find("-->").expect("present") + 3);
    // Inconsistent edit arguments fall back to a full pass.
    assert_eq!(buf.edit("<x/>", 0..999, 1), 0..4);
    assert_eq!(buf.tokens(), tokenize("<x/>").as_slice());
    assert_eq!(buf.tokens_in(1..2).len(), 1);
}

#[test]
fn token_spans_convert_to_utf16() {
    let text = "<a x=\"😀\">ü</a>";
    let tokens = tokenize(text);
    let mut cur = utf16::Utf16Cursor::new(text);
    let ranges: Vec<_> = tokens.iter().map(|t| cur.utf16_range(t.span())).collect();
    let value = tokens
        .iter()
        .position(|t| t.kind == TokenKind::AttrValue)
        .expect("value token");
    assert_eq!(ranges[value], 5..9); // quote, two surrogates, quote
    let text_tok = tokens
        .iter()
        .position(|t| t.kind == TokenKind::Text)
        .expect("text token");
    assert_eq!(ranges[text_tok], 10..11);
}

// ---------------------------------------------------------------- well-formedness

#[test]
fn fixtures_are_well_formed_except_the_broken_one() {
    for (path, text) in all_fixtures() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let result = check_well_formed(&text);
        if name == "invalid-not-well-formed.xml" {
            let d = result.expect_err("must fail");
            assert_eq!(d.source, DiagSource::WellFormedness);
            // Fixture expectation: `<!-- expect: error line 9: well-formed -->`.
            assert_eq!(
                d.pos,
                Some(TextPos {
                    line: 9,
                    column: 19
                }),
                "{d}"
            );
            assert!(d.message.contains("well-formed"), "{d}");
            assert!(
                d.message.contains("tag mismatch") && d.message.contains("line 8"),
                "{d}"
            );
        } else {
            assert_eq!(result, Ok(()), "{path:?}");
        }
    }
}

#[test]
fn well_formedness_errors_and_positions() {
    let cases: &[(&str, u32, u32, &str)] = &[
        (
            "<a><b></a>",
            1,
            11,
            "Opening and ending tag mismatch: b line 1 and a",
        ),
        (
            "<a>\n  <b>\n</a>",
            3,
            5,
            "Opening and ending tag mismatch: b line 2 and a",
        ),
        (
            "<a>\n  <b>\n  <c/>",
            3,
            7,
            "Premature end of data in tag b line 2",
        ),
        ("<a>", 1, 4, "Premature end of data in tag a line 1"),
        ("</a>", 1, 2, "StartTag: invalid element name"),
        ("<p:a/>", 1, 5, "Namespace prefix p on a is not defined"),
        (
            "<a xmlns:q='u'><q:b/><p:c/></a>",
            1,
            26,
            "Namespace prefix p on c is not defined",
        ),
        (
            "<a\n   p:x='1'/>",
            2,
            11,
            "Namespace prefix p for x on a is not defined",
        ),
        ("<a x='1' x='2'/>", 1, 15, "Attribute x redefined"),
        (
            "<a xmlns:p='u' xmlns:q='u' p:x='1' q:x='2'/>",
            1,
            43,
            "Namespaced Attribute x in 'u' redefined",
        ),
        ("<a/><b/>", 1, 5, "Extra content at the end of the document"),
        ("hello<a/>", 1, 1, "Start tag expected, '<' not found"),
        (
            "<a/>\n  x",
            2,
            3,
            "Extra content at the end of the document",
        ),
        ("", 1, 1, "Document is empty"),
        ("<!-- only -->\n", 2, 1, "Start tag expected, '<' not found"),
        ("<a>&nbsp;</a>", 1, 10, "Entity 'nbsp' not defined"),
        (
            "<a>&#0;</a>",
            1,
            8,
            "xmlParseCharRef: invalid xmlChar value 0",
        ),
        (
            "<a>&#xD800;</a>",
            1,
            12,
            "xmlParseCharRef: invalid xmlChar value 55296",
        ),
        ("<a>AT&T</a>", 1, 8, "EntityRef: expecting ';'"),
        ("<a x='AT&T'/>", 1, 11, "EntityRef: expecting ';'"),
        ("<a x='&bogus;'/>", 1, 14, "Entity 'bogus' not defined"),
        (
            "<a x='a<b'/>",
            1,
            8,
            "Unescaped '<' not allowed in attributes values",
        ),
        ("<a x='1'y='2'/>", 1, 9, "attributes construct error"),
        (
            "<a x/>",
            1,
            5,
            "Specification mandates value for attribute x",
        ),
        ("<a x=1/>", 1, 6, "AttValue: \" or ' expected"),
        ("<a>]]></a>", 1, 4, "Sequence ']]>' not allowed in content"),
        (
            "<a><!-- a -- b --></a>",
            1,
            11,
            "Double hyphen within comment: <!-- a",
        ),
        ("<a><!-- x", 1, 10, "Comment not terminated"),
        ("<a><![CDATA[x</a>", 1, 18, "CData section not finished"),
        ("<a x='1'", 1, 9, "attributes construct error"),
        ("<a>\u{1}</a>", 1, 4, "PCDATA invalid Char value 1"),
        ("<a>\u{fffe}</a>", 1, 4, "PCDATA invalid Char value 65534"),
        (
            "<a/>\n<?xml version='1.0'?>",
            2,
            6,
            "XML declaration allowed only at the start of the document",
        ),
        ("< a/>", 1, 2, "StartTag: invalid element name"),
        ("<1a/>", 1, 2, "StartTag: invalid element name"),
        (
            "<a:b:c xmlns:a='u'/>",
            1,
            7,
            "Failed to parse QName 'a:b:c'",
        ),
        (
            "<a xmlns:p=''/>",
            1,
            14,
            "xmlns:p: Empty XML namespace is not allowed",
        ),
        (
            "<a xmlns:xml='urn:x'/>",
            1,
            21,
            "xml namespace prefix mapped to wrong URI",
        ),
        (
            "<a xmlns:xmlns='urn:x'/>",
            1,
            23,
            "redefinition of the xmlns prefix is forbidden",
        ),
        (
            "<xmlns:a/>",
            1,
            9,
            "Namespace prefix xmlns on a is not defined",
        ),
        (
            "<a/><!DOCTYPE a>",
            1,
            5,
            "Extra content at the end of the document",
        ),
        (
            "<a><?xml-stylesheet x?><?XML x?></a>",
            1,
            29,
            "Invalid PI name",
        ),
        (
            "<a>&#x41;ü</a>\u{1}",
            1,
            15,
            "Extra content at the end of the document",
        ),
        (
            "<a>üü€😀</b>",
            1,
            12,
            "Opening and ending tag mismatch: a line 1 and b",
        ),
    ];
    for &(input, line, column, needle) in cases {
        let e = well_formedness_error(input).unwrap_or_else(|| panic!("{input:?} must fail"));
        let d = &e.diagnostic;
        assert!(
            d.message.starts_with("not well-formed: ") && d.message.contains(needle),
            "{input:?}: {d}"
        );
        assert_eq!(d.pos, Some(TextPos { line, column }), "{input:?}: {d}");
        assert_eq!(
            crate::diag::pos_at_byte(input, e.offset),
            TextPos { line, column }
        );
        assert_eq!(d.source, DiagSource::WellFormedness);
    }
}

#[test]
fn well_formed_documents_pass() {
    let ok = [
        "<a/>",
        "<?xml version='1.0' encoding='UTF-8'?>\n<!-- c -->\n<?pi x?>\n<a/>\n<!-- after -->\n",
        "<!DOCTYPE a [<!ENTITY e 'x'>]><a>&e;</a>",
        "<a xmlns='urn:d' xmlns:p='urn:p' p:x='1' x='2' xml:lang='de'><b/><p:c p:y=''/></a>",
        "<a xmlns:p='u' p:x='1'><b xmlns:p='v' p:x='2'/></a>",
        "<a x='&lt;&#60;&#x3C;&quot;&apos;&amp;&gt;'>&lt;&#169;&#x1F600;<![CDATA[<&]]></a>",
        "<a\n  x = \"1\"\n\ty='2' ></a >",
        "<a>]]</a>",
        "<a xmlns:xml='http://www.w3.org/XML/1998/namespace'/>",
        "<a>\u{E000}\u{FFFD}\u{10FFFF}ü</a>",
        "<a b='>'/>",
    ];
    for input in ok {
        assert_eq!(check_well_formed(input), Ok(()), "{input:?}");
    }
}

// ---------------------------------------------------------------- cursor context

fn names(path: &[PathElement]) -> Vec<String> {
    path.iter()
        .map(|e| {
            e.name
                .as_ref()
                .map_or_else(|| format!("?{}", e.raw_name), |n| n.to_string())
        })
        .collect()
}

const CUS: &str = "urn:example:customer";
const COM: &str = "urn:example:common";

#[test]
fn context_in_xsi_type_value() {
    let text = read_fixture("customer/requests/valid-create-order.xml");
    let at = text.find("com:PublicCompany").expect("present") + 4;
    let ctx = cursor_context(&text, at);
    assert_eq!(
        names(&ctx.path),
        [
            format!("{{{SOAP11_ENV_NS}}}Envelope"),
            format!("{{{SOAP11_ENV_NS}}}Body"),
            "{urn:example:customer:messages}CreateOrder".to_owned(),
            format!("{{{CUS}}}Customer"),
            format!("{{{CUS}}}party"),
        ]
    );
    let value_start = at - 4;
    assert_eq!(
        ctx.location,
        CursorLocation::AttributeValue {
            attribute: "xsi:type".into(),
            name: Some(QName::new(XSI_NS, "type")),
            value: value_start..value_start + "com:PublicCompany".len(),
        }
    );
    let party = ctx.tag_element().expect("in a tag");
    assert_eq!(
        party.xsi_type,
        Some(XsiType {
            raw: "com:PublicCompany".into(),
            name: Some(QName::new(COM, "PublicCompany")),
        })
    );
    assert_eq!(party.namespaces.namespace("com"), Some(COM));
    assert_eq!(party.namespaces.prefix_for(XSI_NS), Some("xsi"));
    assert_eq!(party.namespaces.namespace("xml"), Some(XML_NS));
    assert_eq!(ctx.parent_path().len(), 4);
}

#[test]
fn context_in_text_and_names() {
    let text = read_fixture("customer/requests/valid-create-order.xml");
    let at = text.find("office@").expect("present") + 3;
    let ctx = cursor_context(&text, at);
    assert_eq!(ctx.location, CursorLocation::Text);
    assert_eq!(
        names(&ctx.path)[4..],
        [format!("{{{COM}}}Email"), format!("{{{COM}}}address")]
    );
    assert!(ctx.tag_element().is_none());
    assert_eq!(ctx.parent_path().len(), ctx.path.len());
    // xsi:type is per element: the party's does not leak to its children.
    let at = text.find("Huber Logistik").expect("present");
    let ctx = cursor_context(&text, at);
    assert_eq!(
        ctx.path[4].xsi_type.as_ref().map(|t| t.raw.as_str()),
        Some("com:PublicCompany")
    );
    assert_eq!(ctx.path[5].xsi_type, None);

    // An element declaring its own prefix resolves with it, even with the cursor on its name.
    let at = text.find("audit:AuditInfo").expect("present") + 2;
    let ctx = cursor_context(&text, at);
    let start = at - 2;
    assert_eq!(
        ctx.location,
        CursorLocation::ElementName {
            closing: false,
            name: start..start + "audit:AuditInfo".len(),
        }
    );
    let el = ctx.tag_element().expect("in tag");
    assert_eq!(el.name, Some(QName::new("urn:example:audit", "AuditInfo")));
    assert_eq!(el.start, start - 1);

    // End tag name.
    let at = text.find("</cus:Order>").expect("present") + 5;
    let ctx = cursor_context(&text, at);
    assert!(matches!(
        ctx.location,
        CursorLocation::ElementName { closing: true, .. }
    ));
    assert_eq!(
        names(&ctx.path).last().map(String::as_str),
        Some("{urn:example:customer}Order")
    );

    // Attribute name, and the gap between attributes.
    let at = text.find("sku=").expect("present") + 1;
    let ctx = cursor_context(&text, at);
    assert_eq!(
        ctx.location,
        CursorLocation::AttributeName {
            name: at - 1..at + 2
        }
    );
    let at = text.find(" qty=").expect("present") + 1;
    assert!(matches!(
        cursor_context(&text, at).location,
        CursorLocation::AttributeName { .. }
    ));
    let at = text.find("\"3\"").expect("present") + 3;
    assert_eq!(cursor_context(&text, at).location, CursorLocation::Tag);
}

#[test]
fn context_outside_and_in_comments() {
    let text = read_fixture("customer/requests/valid-create-order.xml");
    assert_eq!(cursor_context(&text, 0).location, CursorLocation::Outside);
    assert_eq!(cursor_context(&text, 5).location, CursorLocation::Comment);
    assert!(cursor_context(&text, 5).path.is_empty());
    assert_eq!(
        cursor_context(&text, text.len()).location,
        CursorLocation::Outside
    );
    assert!(cursor_context(&text, text.len()).path.is_empty());
    assert_eq!(
        cursor_context(&text, usize::MAX).location,
        CursorLocation::Outside
    );
    let ctx = cursor_context("<?xml version='1.0'?><a><![CDATA[x]]></a>", 4);
    assert_eq!(ctx.location, CursorLocation::Markup);
    let ctx = cursor_context("<?xml version='1.0'?><a><![CDATA[x]]></a>", 34);
    assert_eq!(ctx.location, CursorLocation::CData);
    assert_eq!(names(&ctx.path), ["a"]);
}

#[test]
fn context_while_typing() {
    let head = "<e:Envelope xmlns:e=\"urn:e\" xmlns=\"urn:d\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n  <e:Body>\n    <Op xsi:type=\"T\">\n      ";
    // Just typed `<`: element-name completion in the parent.
    let text = format!("{head}<");
    let ctx = cursor_context(&text, text.len());
    assert_eq!(
        ctx.location,
        CursorLocation::ElementName {
            closing: false,
            name: text.len()..text.len()
        }
    );
    assert!(ctx.tag_element().is_none());
    assert_eq!(
        names(&ctx.path),
        ["{urn:e}Envelope", "{urn:e}Body", "{urn:d}Op"]
    );
    let ns = &ctx.path[2].namespaces;
    assert_eq!(ns.prefix_for("urn:d"), Some(""));
    assert_eq!(ns.declared_prefix_for("urn:d"), None);
    assert_eq!(ns.prefix_for("urn:e"), Some("e"));
    assert_eq!(ns.prefix_for("urn:unknown"), None);
    // Unprefixed xsi:type values take the default namespace.
    assert_eq!(
        ctx.path[2].xsi_type.as_ref().and_then(|t| t.name.clone()),
        Some(QName::new("urn:d", "T"))
    );

    // Partial name followed by garbage and an unrelated broken tail.
    let text = format!("{head}<Fo <<</Bar> & \"");
    let at = head.len() + 3;
    let ctx = cursor_context(&text, at);
    assert_eq!(
        ctx.location,
        CursorLocation::ElementName {
            closing: false,
            name: at - 2..at
        }
    );
    assert_eq!(
        names(ctx.parent_path()),
        ["{urn:e}Envelope", "{urn:e}Body", "{urn:d}Op"]
    );
    assert_eq!(
        ctx.tag_element().and_then(|e| e.name.clone()),
        Some(QName::new("urn:d", "Fo"))
    );

    // Inside a start tag at the end of input: attribute name position.
    let text = format!("{head}<Foo ");
    let ctx = cursor_context(&text, text.len());
    assert_eq!(
        ctx.location,
        CursorLocation::AttributeName {
            name: text.len()..text.len()
        }
    );
    // Inside an unterminated attribute value.
    let text = format!("{head}<Foo xsi:type=\"e:");
    let ctx = cursor_context(&text, text.len());
    assert!(
        matches!(ctx.location, CursorLocation::AttributeValue { ref attribute, .. } if attribute == "xsi:type")
    );
    assert_eq!(
        ctx.tag_element()
            .and_then(|e| e.xsi_type.as_ref())
            .and_then(|t| t.name.clone()),
        Some(QName::new("urn:e", ""))
    );
    // `</` at the end: closing-name completion, element being closed is the last entry.
    let text = format!("{head}</");
    let ctx = cursor_context(&text, text.len());
    assert!(matches!(
        ctx.location,
        CursorLocation::ElementName { closing: true, .. }
    ));
    assert_eq!(ctx.tag_element().map(|e| e.raw_name.as_str()), Some("Op"));
    // Undeclared prefixes resolve to None instead of failing.
    let text = format!("{head}<zz:Foo>");
    let ctx = cursor_context(&text, text.len());
    assert_eq!(names(&ctx.path).last().map(String::as_str), Some("?zz:Foo"));
}

fn names_of(path: &[PathElement]) -> Vec<String> {
    path.iter().map(|e| e.raw_name.clone()).collect()
}

/// Content positions depend only on the text before the cursor: truncating after it changes
/// nothing (that is what "works on documents broken after the cursor" means).
#[test]
fn context_ignores_text_after_cursor() {
    for (path, text) in all_fixtures() {
        for (at, _) in text.char_indices().step_by(7) {
            let full = cursor_context(&text, at);
            if !matches!(
                full.location,
                CursorLocation::Text | CursorLocation::Outside
            ) {
                continue;
            }
            let cut = cursor_context(&text[..at], at);
            assert_eq!(
                names_of(&full.path),
                names_of(&cut.path),
                "{path:?} at {at}"
            );
            assert_eq!(full.path, cut.path, "{path:?} at {at}");
        }
    }
}

// ---------------------------------------------------------------- pretty-printer

#[test]
fn pretty_prints_element_only_content() {
    let input = "<?xml version=\"1.0\"?><!--top--><a xmlns=\"urn:a\"   x = 'v&amp;'><b><c/><!-- note --><?pi x?></b>\
                 <d>  text  &lt; <i>mixed</i> </d><e> </e><f></f><g><![CDATA[ x ]]></g></a>";
    let expected = "<?xml version=\"1.0\"?>\n<!--top-->\n<a xmlns=\"urn:a\" x='v&amp;'>\n  <b>\n    <c/>\n    \
                    <!-- note -->\n    <?pi x?>\n  </b>\n  <d>  text  &lt; <i>mixed</i> </d>\n  <e> </e>\n  \
                    <f></f>\n  <g><![CDATA[ x ]]></g>\n</a>\n";
    let out = pretty_print(input).expect("well-formed");
    assert_eq!(out, expected);
    assert_eq!(pretty_print(&out).expect("well-formed"), out, "idempotent");
}

#[test]
fn pretty_keeps_formatted_fixtures_unchanged() {
    for rel in [
        "customer/requests/valid-get-customer.xml",
        "customer/requests/valid-create-order.xml",
        "customer/requests/invalid-unknown-operation.xml",
        "legacy-rpc/requests/valid-lookup.xml",
    ] {
        let text = read_fixture(rel);
        assert_eq!(pretty_print(&text).expect("well-formed"), text, "{rel}");
    }
}

#[test]
fn pretty_reindents_and_aligns_attribute_lines() {
    let input = "<s:E xmlns:s=\"urn:s\"\n      xmlns:m=\"urn:m\" a=\"1\">\n<s:B>\n        <m:Op><m:x>1</m:x></m:Op></s:B></s:E>";
    let expected = "<s:E xmlns:s=\"urn:s\"\n     xmlns:m=\"urn:m\" a=\"1\">\n  <s:B>\n    <m:Op>\n      <m:x>1</m:x>\n    </m:Op>\n  </s:B>\n</s:E>\n";
    assert_eq!(pretty_print(input).expect("well-formed"), expected);
    // CRLF input keeps CRLF.
    let out = pretty_print("<a>\r\n<b/></a>").expect("well-formed");
    assert_eq!(out, "<a>\r\n  <b/>\r\n</a>\r\n");
    // Every fixture formats to something well-formed and stable.
    for (path, text) in all_fixtures() {
        let Ok(out) = pretty_print(&text) else {
            continue;
        };
        assert_eq!(check_well_formed(&out), Ok(()), "{path:?}");
        assert_eq!(pretty_print(&out).as_ref(), Ok(&out), "{path:?}");
    }
}

#[test]
fn pretty_refuses_broken_xml() {
    let text = read_fixture("customer/requests/invalid-not-well-formed.xml");
    let d = pretty_print(&text).expect_err("not well-formed");
    assert_eq!(d.pos.map(|p| p.line), Some(9));
}

// ---------------------------------------------------------------- robustness

#[test]
fn nothing_panics_on_truncated_and_mutated_fixtures() {
    let mut rng = fastrand::Rng::with_seed(0xDEAD_BEEF_CAFE_F00D);
    for (_, text) in all_fixtures() {
        let mut inputs: Vec<String> = Vec::new();
        for _ in 0..40 {
            let cut = boundary(&mut rng, &text);
            inputs.push(text[..cut].to_owned());
            let a = boundary(&mut rng, &text);
            let mut s = text[..a].to_owned();
            for _ in 0..rng.usize(..4) + 1 {
                s.push_str(SNIPPETS[rng.usize(..SNIPPETS.len())]);
            }
            let b = boundary(&mut rng, &text).max(a);
            let b = (a..=b)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(a);
            s.push_str(&text[b..]);
            inputs.push(s);
        }
        for s in &inputs {
            exercise(s, &mut rng);
        }
    }
    for s in [
        "",
        "<",
        "</",
        "<!",
        "<?",
        "<!--",
        "<![CDATA[",
        "<!DOCTYPE",
        "&",
        "&#",
        "&#x;",
        "<a",
        "<a x",
        "<a x=",
        "<a x='",
        "\u{FEFF}",
        "<a>\u{0}</a>",
        "<:/>",
        "<a:/>",
        "<:a/>",
    ] {
        exercise(s, &mut rng);
    }
}

fn exercise(s: &str, rng: &mut fastrand::Rng) {
    let tokens = tokenize(s);
    assert_token_invariants(s, &tokens);
    let wf = well_formedness_error(s);
    if let Some(e) = &wf {
        assert!(e.offset <= s.len());
    }
    let pretty = pretty_print(s);
    assert_eq!(pretty.is_ok(), wf.is_none(), "{s:?}");
    for _ in 0..6 {
        let at = rng.usize(..s.len() + 2);
        let ctx = cursor_context(s, at);
        assert!(ctx.parent_path().len() <= ctx.path.len());
    }
    let mut buf = TokenBuffer::new(s);
    let at = boundary(rng, s);
    let new_text = format!("{}<!--{}", &s[..at], &s[at..]);
    buf.edit(&new_text, at..at, 4);
    assert_eq!(buf.tokens(), tokenize(&new_text).as_slice());
    let u = utf16::utf16_len(s);
    assert_eq!(utf16::byte_to_utf16(s, s.len()), u);
    assert_eq!(utf16::utf16_to_byte(s, u), s.len());
}

#[test]
fn deep_nesting_does_not_overflow() {
    let depth = 100_000;
    // libxml2 refuses nesting deeper than 256 (raising that needs XML_PARSE_HUGE, which also
    // lifts its limits on text and entity sizes); the editor and validation agree on that.
    let mut s = String::new();
    for _ in 0..depth {
        s.push_str("<a>");
    }
    s.push('x');
    for _ in 0..depth {
        s.push_str("</a>");
    }
    let e = check_well_formed(&s).expect_err("too deep");
    assert!(e.message.contains("Excessive depth"), "{e}");
    assert_eq!(cursor_context(&s, depth * 3).path.len(), depth);
    assert_eq!(tokenize(&s).len(), depth * 6 + 1);
    // The formatter only accepts well-formed input, so its depth is bounded by the same limit.
    let depth = 250;
    let s = format!("{}x{}", "<a><b/>".repeat(depth), "</a>".repeat(depth));
    let out = pretty_print(&s).expect("well-formed");
    assert_eq!(check_well_formed(&out), Ok(()));
}

// ---------------------------------------------------------------- performance

/// A ~1 MB SOAP request: namespaces, attributes, xsi:type, comments, entities, CDATA.
fn large_document(target: usize) -> String {
    let mut s = String::with_capacity(target + 4096);
    s.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"\n\
         \x20                 xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n\
         \x20                 xmlns:cus=\"urn:example:customer\" xmlns:com=\"urn:example:common\">\n\
         \x20 <soapenv:Body>\n    <cus:Batch>\n",
    );
    let mut i = 0u32;
    while s.len() < target {
        i += 1;
        s.push_str(&format!(
            "      <cus:Customer id=\"c-{i}\" status=\"ACTIVE\">\n\
             \x20       <!-- customer {i} -->\n\
             \x20       <cus:party xsi:type=\"com:Company\">\n\
             \x20         <com:displayName>Huber &amp; Söhne Logistik {i}</com:displayName>\n\
             \x20         <com:country>AT</com:country>\n\
             \x20         <com:note><![CDATA[a < b & c]]></com:note>\n\
             \x20       </cus:party>\n\
             \x20       <cus:line sku=\"A-{i}\" qty=\"3\"/>\n\
             \x20     </cus:Customer>\n"
        ));
    }
    s.push_str("    </cus:Batch>\n  </soapenv:Body>\n</soapenv:Envelope>\n");
    s
}

fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// Acceptance: well-formedness check and full tokenization of 1 MB under 20 ms each in a
/// release build. Debug builds only report the numbers. Run with
/// `cargo test --release -p washboard-core -- --nocapture perf_1mb`.
#[test]
fn perf_1mb() {
    let doc = large_document(1 << 20);
    assert_eq!(check_well_formed(&doc), Ok(()));
    let wf = best_of(5, || check_well_formed(&doc));
    let tok = best_of(5, || tokenize(&doc));
    let ctx = best_of(5, || cursor_context(&doc, doc.len() - 100));
    let mut buf = TokenBuffer::new(&doc);
    let at = doc.len() / 2;
    let edited = format!("{}x{}", &doc[..at], &doc[at..]);
    let edit = best_of(1, || buf.edit(&edited, at..at, 1));
    eprintln!(
        "perf_1mb ({} bytes, {} tokens): well-formedness {wf:?}, tokenize {tok:?}, \
         cursor_context at end {ctx:?}, one-char edit {edit:?}",
        doc.len(),
        buf.tokens().len()
    );
    if !cfg!(debug_assertions) {
        assert!(
            wf < Duration::from_millis(20),
            "well-formedness took {wf:?}"
        );
        assert!(tok < Duration::from_millis(20), "tokenize took {tok:?}");
    }
}
