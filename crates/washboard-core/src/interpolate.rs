//! `${name}` placeholders in request text (`docs/specs/parameters.md`).
//!
//! [`resolve`] fills them in from a [`Source`] and returns the resolved text with a map from
//! every replacement back to the placeholder it came from, so diagnostics found in the resolved
//! text can point at the request the user wrote ([`Resolved::map_diagnostics`]). The request
//! file keeps its placeholders; only what is validated and sent is resolved, and the send path
//! resolves once and sends exactly that text.
//!
//! Syntax:
//! - `${name}` with a [valid name](is_name) is a placeholder;
//! - anything else between `${` and the next `}` is an "invalid placeholder" error, never
//!   literal text, so later extensions (arguments, say) cannot change what existing requests
//!   send;
//! - `$${` is a literal `${`; a `${` without a closing `}`, or a bare `$`, is literal text.
//!
//! Values are inserted XML-escaped and never expanded again.

use std::ops::Range;

use crate::diag::{DiagSource, Diagnostic, LineIndex};
use crate::model::ParameterSet;

/// What a [`Source`] knows about one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The value, unescaped, and the parameter set it comes from (for messages).
    Value { value: String, set: Option<String> },
    /// No variable of that name. Dotted names are reserved for built-in values, so they are
    /// undefined until those exist.
    Undefined,
    /// The variable exists, but no parameter set is selected.
    NoSet,
    /// The variable exists, but the selected set has no value for it.
    Unset { set: String },
}

/// Where placeholder values come from: a lookup by name rather than a fixed map, so built-in
/// generated values can be added beside the project's parameters later.
pub trait Source {
    fn lookup(&self, name: &str) -> Lookup;
}

/// The project's variables and the selected set: what a send resolves against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Values {
    /// Every variable of the project, set or not.
    pub variables: Vec<String>,
    pub set: Option<ParameterSet>,
}

impl Source for Values {
    fn lookup(&self, name: &str) -> Lookup {
        if !self.variables.iter().any(|v| v == name) {
            return Lookup::Undefined;
        }
        match &self.set {
            None => Lookup::NoSet,
            Some(set) => match set.values.get(name) {
                Some(value) => Lookup::Value {
                    value: value.clone(),
                    set: Some(set.name.clone()),
                },
                None => Lookup::Unset {
                    set: set.name.clone(),
                },
            },
        }
    }
}

/// A placeholder name: `[A-Za-z_][A-Za-z0-9_]*`, optionally followed by `.`-separated parts
/// of `[A-Za-z0-9_]+`. Names with a `.` are reserved for built-in values.
pub fn is_name(name: &str) -> bool {
    let starts_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_ok
        && name.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// A name a project variable may have: a [placeholder name](is_name) without a `.`.
pub fn is_user_name(name: &str) -> bool {
    is_name(name) && !name.contains('.')
}

/// One stretch of the source that the resolved text replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    source: Range<usize>,
    resolved: Range<usize>,
    /// `None` for the `$${` escape.
    value: Option<Filled>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Filled {
    name: String,
    value: String,
    set: Option<String>,
}

/// The result of [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    /// The text with every placeholder that could be filled in replaced. Placeholders that
    /// could not stay as written.
    pub text: String,
    /// One error per placeholder that could not be filled in, positioned in the source.
    pub diagnostics: Vec<Diagnostic>,
    replacements: Vec<Replacement>,
}

impl Resolved {
    /// Validating the resolved text is pointless while this is true, and send is blocked.
    pub fn has_errors(&self) -> bool {
        crate::diag::has_errors(&self.diagnostics)
    }

    /// Whether the text had anything to replace; if not, `text` equals the source.
    pub fn is_identity(&self) -> bool {
        self.replacements.is_empty()
    }

    /// Moves diagnostics found in [`Self::text`] to the source they were resolved from.
    /// One that falls inside a filled-in value covers the whole placeholder and says which
    /// value it was.
    pub fn map_diagnostics(&self, source: &str, diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
        if self.is_identity() {
            return diags;
        }
        let from = LineIndex::new(&self.text);
        let to = LineIndex::new(source);
        diags
            .into_iter()
            .map(|d| self.map_diagnostic(&from, &to, d))
            .collect()
    }

    /// An end or start inside a value widens the span to the whole placeholder, and every value
    /// an end, the start or the position falls inside is named, in source order. A span that
    /// merely encloses a value names none: it is about more than the value.
    fn map_diagnostic(
        &self,
        from: &LineIndex<'_>,
        to: &LineIndex<'_>,
        mut d: Diagnostic,
    ) -> Diagnostic {
        let mut named: Vec<&Replacement> = Vec::new();
        let span = d.span.map(|s| {
            let (start, end) = (from.byte_clamped(s.start), from.byte_clamped(s.end));
            if let Some(r) = self.empty_value_at(start, end) {
                named.push(r);
                return r.source.clone();
            }
            let (start, a) = self.map_byte(start, false);
            let (end, b) = self.map_byte(end, true);
            named.extend(a.into_iter().chain(b));
            start.start..end.end
        });
        let pos = d.pos.map(|p| {
            let byte = from.byte_clamped(p);
            if d.span.is_none()
                && let Some(r) = self.empty_value_at(byte, byte)
            {
                named.push(r);
                return r.source.start;
            }
            let (at, inside) = self.map_byte(byte, false);
            named.extend(inside);
            at.start
        });
        named.sort_by_key(|r| r.source.start);
        named.dedup_by_key(|r| r.source.start);
        d.span = span.clone().map(|s| to.span(s));
        // A diagnostic about a value starts where its span now starts: the position libxml2
        // gives (the element's start tag) is no closer to the placeholder than that.
        d.pos = match span {
            Some(s) if !named.is_empty() => Some(to.pos(s.start)),
            _ => pos.map(|p| to.pos(p)),
        };
        let values: Vec<String> = named
            .iter()
            .filter_map(|r| r.value.as_ref())
            .map(|f| match &f.set {
                Some(set) => format!("{} = {:?} in “{set}”", f.name, f.value),
                None => format!("{} = {:?}", f.name, f.value),
            })
            .collect();
        if !values.is_empty() {
            d.message.push_str(&format!(" ({})", values.join(", ")));
        }
        d
    }

    /// The empty value a diagnostic over resolved bytes `start..end` is about, if any.
    ///
    /// An empty value leaves nothing in the resolved text for a diagnostic to fall inside, so
    /// "touching" has to do, and the side matters. libxml2 reports an element with empty content
    /// at its start tag, so a span that ends with a `>` exactly where an empty value went in is
    /// about that value; so is a zero-width diagnostic there. A span that *starts* there is
    /// about what follows the value (a sibling's start tag, say), and one ending there after
    /// text is about that text. The price is that an error about a complex element whose
    /// content starts with an empty value is pinned on that value; that layout is rare, as
    /// such a value would be mixed content.
    fn empty_value_at(&self, start: usize, end: usize) -> Option<&Replacement> {
        let r = self
            .replacements
            .iter()
            .find(|r| r.resolved.is_empty() && r.resolved.start == end)?;
        (start == end || self.text[..end].ends_with('>')).then_some(r)
    }

    /// The source range of resolved offset `byte`, and the replacement it falls inside: the
    /// whole placeholder for an offset inside one, else the offset shifted (an empty range).
    /// `end` treats `byte` as an exclusive end, so an end right after a replacement, or at an
    /// empty one, is not inside it and stays before it.
    fn map_byte(&self, byte: usize, end: bool) -> (Range<usize>, Option<&Replacement>) {
        let mut shift: isize = 0;
        for r in &self.replacements {
            let inside = if end {
                r.resolved.start < byte && byte < r.resolved.end
            } else {
                r.resolved.start <= byte && byte < r.resolved.end
            };
            if inside {
                return (r.source.clone(), Some(r));
            }
            if byte < r.resolved.end || (end && byte == r.resolved.start) {
                break;
            }
            shift = r.source.end as isize - r.resolved.end as isize;
        }
        let at = byte.saturating_add_signed(shift);
        (at..at, None)
    }
}

/// Fills in the placeholders of `source` from `values`. See the module documentation for the
/// syntax.
pub fn resolve(source: &str, values: &dyn Source) -> Resolved {
    let mut out = Resolved {
        text: String::with_capacity(source.len()),
        ..Resolved::default()
    };
    let mut lines: Option<LineIndex<'_>> = None;
    let mut error = |range: Range<usize>, message: String, out: &mut Resolved| {
        let lines = lines.get_or_insert_with(|| LineIndex::new(source));
        let span = lines.span(range);
        out.diagnostics.push(
            Diagnostic::error(DiagSource::Parameters, Some(span.start), message).with_span(span),
        );
    };
    let mut copied = 0;
    let mut i = 0;
    while let Some(rel) = source[i..].find('$') {
        let at = i + rel;
        let rest = &source[at..];
        if rest.starts_with("$${") {
            out.text.push_str(&source[copied..at]);
            let start = out.text.len();
            out.text.push_str("${");
            out.replacements.push(Replacement {
                source: at..at + 3,
                resolved: start..out.text.len(),
                value: None,
            });
            i = at + 3;
            copied = i;
            continue;
        }
        if !rest.starts_with("${") {
            i = at + 1;
            continue;
        }
        let Some(close) = source[at + 2..].find('}') else {
            // Literal text. No `${` after it can close either, but a `$${` is still an escape.
            i = at + 2;
            continue;
        };
        let end = at + 2 + close + 1;
        let name = &source[at + 2..end - 1];
        i = end;
        if !is_name(name) {
            error(
                at..end,
                format!("invalid placeholder: {}", shorten(&source[at..end])),
                &mut out,
            );
            continue;
        }
        let missing = match values.lookup(name) {
            Lookup::Value { value, set } => {
                out.text.push_str(&source[copied..at]);
                let start = out.text.len();
                push_escaped(&mut out.text, &value);
                out.replacements.push(Replacement {
                    source: at..end,
                    resolved: start..out.text.len(),
                    value: Some(Filled {
                        name: name.to_owned(),
                        value,
                        set,
                    }),
                });
                copied = end;
                continue;
            }
            Lookup::Undefined => format!("undefined parameter: {name}"),
            Lookup::NoSet => {
                format!("parameter {name} has no value: no parameter set selected")
            }
            Lookup::Unset { set } => format!("parameter {name} has no value in “{set}”"),
        };
        error(at..end, missing, &mut out);
    }
    out.text.push_str(&source[copied..]);
    out
}

/// Escapes for both element content and quoted attribute values. Tab, line feed and carriage
/// return become character references, which attribute value normalization would otherwise
/// turn into spaces.
fn push_escaped(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
}

/// An invalid placeholder as quoted in its message: the first line, at most 40 characters.
fn shorten(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    let mut out: String = line.chars().take(40).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::diag::TextPos;
    use crate::model::ParameterSetId;

    fn values(set: Option<&[(&str, &str)]>) -> Values {
        Values {
            variables: vec!["id".into(), "name".into(), "unset".into()],
            set: set.map(|vals| ParameterSet {
                id: ParameterSetId::new(),
                name: "Customer A".into(),
                values: vals
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect::<BTreeMap<_, _>>(),
            }),
        }
    }

    fn a() -> Values {
        values(Some(&[("id", "4711"), ("name", "Smith & <Sons> \"Q\"")]))
    }

    fn messages(r: &Resolved) -> Vec<&str> {
        r.diagnostics.iter().map(|d| d.message.as_str()).collect()
    }

    #[test]
    fn names() {
        for ok in ["a", "_x", "customer_id", "A1", "now.iso", "uuid.v4", "a.1"] {
            assert!(is_name(ok), "{ok:?}");
        }
        for bad in ["", "1x", "a-b", "a.", ".a", "a..b", " a", "ä"] {
            assert!(!is_name(bad), "{bad:?}");
        }
        assert!(is_user_name("customer_id"));
        assert!(!is_user_name("now.iso"));
    }

    #[test]
    fn fills_in_and_escapes() {
        let r = resolve("<a id=\"${id}\">${name}</a>", &a());
        assert_eq!(r.diagnostics, []);
        assert_eq!(
            r.text,
            "<a id=\"4711\">Smith &amp; &lt;Sons&gt; &quot;Q&quot;</a>"
        );
        let r = resolve("<a>${id}\n</a>", &values(Some(&[("id", "a\tb\nc")])));
        assert_eq!(r.text, "<a>a&#9;b&#10;c\n</a>");
    }

    #[test]
    fn values_are_not_expanded_again() {
        let r = resolve("${id}", &values(Some(&[("id", "${name}")])));
        assert_eq!(r.text, "${name}");
        assert_eq!(r.diagnostics, []);
    }

    #[test]
    fn literal_text_and_escape() {
        let src = "$ $x $$ ${unclosed";
        let r = resolve(src, &a());
        assert_eq!(r.text, src);
        assert!(r.is_identity());
        assert_eq!(r.diagnostics, []);

        let r = resolve("$${id} $$${id}", &a());
        assert_eq!(r.text, "${id} $${id}");
        assert_eq!(r.diagnostics, []);

        // An unclosed `${` does not end the scan: escapes after it still count.
        let r = resolve("${a $${b", &a());
        assert_eq!(r.text, "${a ${b");
        assert_eq!(r.diagnostics, []);
    }

    #[test]
    fn problems_are_errors_at_the_placeholder() {
        let r = resolve(
            "<a>\n  ${1x} ${nope} ${now.iso} ${unset}\n</a>",
            &values(Some(&[])),
        );
        assert_eq!(
            messages(&r),
            [
                "invalid placeholder: ${1x}",
                "undefined parameter: nope",
                "undefined parameter: now.iso",
                "parameter unset has no value in “Customer A”",
            ]
        );
        let d = &r.diagnostics[0];
        assert_eq!(d.source, DiagSource::Parameters);
        assert!(d.is_error());
        assert_eq!(d.pos, Some(TextPos { line: 2, column: 3 }));
        let span = d.span.expect("span");
        assert_eq!(span.end, TextPos { line: 2, column: 8 });
        // Unresolved placeholders stay as written.
        assert_eq!(r.text, "<a>\n  ${1x} ${nope} ${now.iso} ${unset}\n</a>");
        assert!(r.has_errors());

        let r = resolve("${id}", &values(None));
        assert_eq!(
            messages(&r),
            ["parameter id has no value: no parameter set selected"]
        );
    }

    #[test]
    fn invalid_placeholders_are_shortened() {
        let r = resolve("${ a\nb }", &a());
        assert_eq!(messages(&r), ["invalid placeholder: ${ a…"]);
    }

    fn diag(text: &str, span: Range<usize>) -> Diagnostic {
        let lines = LineIndex::new(text);
        let s = lines.span(span);
        Diagnostic::error(DiagSource::Schema, Some(s.start), "bad").with_span(s)
    }

    #[test]
    fn diagnostics_map_back_to_the_source() {
        let src = "<a>${id}</a>\n<b>x</b><c>${name}</c>";
        let r = resolve(src, &a());
        let at = |needle: &str| r.text.find(needle).expect(needle);

        // After a replacement: shifted back.
        let b = at("<b>");
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, b..b + 3)]);
        let lines = LineIndex::new(src);
        assert_eq!(mapped[0].span, Some(lines.span(13..16)));
        assert_eq!(mapped[0].message, "bad");

        // Inside a value: the whole placeholder, with the value named.
        let v = at("4711");
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, v..v + 4)]);
        assert_eq!(mapped[0].span, Some(lines.span(3..8)));
        assert_eq!(mapped[0].message, "bad (id = \"4711\" in “Customer A”)");

        // An element span around a value ends after the placeholder.
        let c = at("<c>");
        let close = at("</c>") + 4;
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, c..close)]);
        let src_c = src.find("<c>").expect("c");
        assert_eq!(mapped[0].span, Some(lines.span(src_c..src.len())));
        assert_eq!(mapped[0].message, "bad");
    }

    #[test]
    fn diagnostics_across_values_cover_their_placeholders() {
        let src = "<c>${id} ${name}</c>";
        let r = resolve(src, &a());
        let at = |needle: &str| r.text.find(needle).expect(needle);
        let lines = LineIndex::new(src);
        let id = src.find("${id}").expect("id");
        let name_end = src.find("</c>").expect("end");
        let both = "(id = \"4711\" in “Customer A”, \
                    name = \"Smith & <Sons> \\\"Q\\\"\" in “Customer A”)";

        // From inside one value to inside the next: both placeholders, both values named.
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, at("711")..at("Sons"))]);
        assert_eq!(mapped[0].span, Some(lines.span(id..name_end)));
        assert_eq!(mapped[0].pos, Some(lines.pos(id)));
        assert_eq!(mapped[0].message, format!("bad {both}"));

        // From before a value to inside it: up to the end of its placeholder.
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, 0..at("711"))]);
        assert_eq!(mapped[0].span, Some(lines.span(0..id + 5)));
        assert_eq!(mapped[0].pos, Some(lines.pos(0)));
        assert_eq!(mapped[0].message, "bad (id = \"4711\" in “Customer A”)");
    }

    #[test]
    fn diagnostics_at_an_empty_value_cover_its_placeholder() {
        let src = "<a>${name}</a><b>${name}<c/></b><d>x${name}</d>";
        let r = resolve(src, &values(Some(&[("name", "")])));
        assert_eq!(r.text, "<a></a><b><c/></b><d>x</d>");
        let at = |needle: &str| r.text.find(needle).expect(needle);
        let lines = LineIndex::new(src);
        let first = lines.span(3..10);
        let named = "bad (name = \"\" in “Customer A”)";

        // An element whose content is just the empty value is reported at its start tag, which
        // ends where the value would be.
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, 0..3)]);
        assert_eq!(mapped[0].span, Some(first));
        assert_eq!(mapped[0].pos, Some(first.start));
        assert_eq!(mapped[0].message, named);

        // A zero-width diagnostic there, too.
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, 3..3)]);
        assert_eq!(mapped[0].span, Some(first));
        assert_eq!(mapped[0].message, named);

        // What starts right after an empty value is about what follows.
        let c = at("<c/>");
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, c..c + 4)]);
        let src_c = src.find("<c/>").expect("c");
        assert_eq!(mapped[0].span, Some(lines.span(src_c..src_c + 4)));
        assert_eq!(mapped[0].message, "bad");

        // Text ending where an empty value starts is not about the value.
        let x = at("x");
        let mapped = r.map_diagnostics(src, vec![diag(&r.text, x..x + 1)]);
        let src_x = src.find('x').expect("x");
        assert_eq!(mapped[0].span, Some(lines.span(src_x..src_x + 1)));
        assert_eq!(mapped[0].message, "bad");
    }

    #[test]
    fn no_placeholders_map_to_themselves() {
        let src = "<a>x</a>";
        let r = resolve(src, &a());
        assert!(r.is_identity());
        let d = diag(src, 0..3);
        assert_eq!(r.map_diagnostics(src, vec![d.clone()]), [d]);
    }
}
