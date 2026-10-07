//! XML text utilities shared by all modules.
//!
//! Every XML input (WSDL, XSD, request files, responses) goes through [`decode`] before it is
//! handed to a Rust XML parser. `roxmltree` and `quick-xml` want `&str` without a BOM; real
//! WSDLs come with UTF-8 BOMs and occasionally as UTF-16 or ISO-8859-1.
//!
//! libxml2 gets the original bytes instead — it handles encodings itself — so positions it
//! reports are converted via [`crate::diag::TextPos`], which does not count the BOM either.
//!
//! Editor-side helpers (all operate on the decoded `&str`, positions are byte offsets into it):
//! - [`tokenize`], [`tokenize_range`], [`TokenBuffer`]: tolerant tokenizer for highlighting;
//! - [`check_well_formed`]: first well-formedness error with its position;
//! - [`cursor_context`]: element path, namespaces and `xsi:type` at the cursor, for completion;
//! - [`pretty_print`]: "Format XML";
//! - [`utf16`]: byte ↔ UTF-16 offset conversion, because AppKit ranges are UTF-16.
//!
//! None of these panic on any input; users type into the editor, so broken XML is the norm.

mod context;
mod encoding;
mod escape;
mod lex;
mod names;
mod pretty;
mod start_tags;
mod tokens;
pub mod utf16;
mod wellformed;

pub use context::{CursorContext, CursorLocation, PathElement, XsiType, cursor_context};
pub use encoding::{DecodeError, Decoded, Encoding, decode, encode_utf8};
pub use escape::{escape_attr, escape_text};
pub use names::NamespaceMap;
pub use pretty::pretty_print;
pub use start_tags::{StartTag, start_tag_at, start_tags};
pub use tokens::{RangeTokens, Token, TokenBuffer, TokenKind, tokenize, tokenize_range};
pub use wellformed::{WellFormednessError, check_well_formed, well_formedness_error};

/// Namespace bound to the `xml` prefix by definition.
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// Namespace of `xmlns` attributes; it may not be bound to any prefix.
pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

#[cfg(test)]
mod tests;
