//! XML text utilities shared by all modules.
//!
//! Every XML input (WSDL, XSD, request files, responses) goes through [`decode`] before it is
//! handed to a Rust XML parser. `roxmltree` and `quick-xml` want `&str` without a BOM; real
//! WSDLs come with UTF-8 BOMs and occasionally as UTF-16 or ISO-8859-1.
//!
//! libxml2 gets the original bytes instead — it handles encodings itself — so positions it
//! reports are converted via [`crate::diag::TextPos`], which does not count the BOM either.
//!
//! Planned additions (work package WP-XML in `docs/TASKS.md`): tokenizer for highlighting,
//! well-formedness check with positions, pretty-printer.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Latin1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The document text without BOM.
    pub text: String,
    pub encoding: Encoding,
    pub had_bom: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unsupported encoding {0:?} in XML declaration")]
    UnsupportedEncoding(String),
    #[error("invalid {encoding:?} data at byte {offset}")]
    Invalid { encoding: Encoding, offset: usize },
}

/// Decodes raw XML bytes to text, honouring a BOM and the XML declaration's `encoding`.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return utf8(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, Encoding::Utf16Le, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, Encoding::Utf16Be, true);
    }
    // UTF-16 without BOM: "<?" encoded as 3C 00 3F 00 / 00 3C 00 3F.
    if bytes.starts_with(&[0x3C, 0x00, 0x3F, 0x00]) {
        return utf16(bytes, Encoding::Utf16Le, false);
    }
    if bytes.starts_with(&[0x00, 0x3C, 0x00, 0x3F]) {
        return utf16(bytes, Encoding::Utf16Be, false);
    }
    match declared_encoding(bytes).map(|e| e.to_ascii_lowercase()) {
        None => utf8(bytes, false),
        Some(e) if e == "utf-8" || e == "utf8" || e == "us-ascii" || e == "ascii" => {
            utf8(bytes, false)
        }
        Some(e) if e == "iso-8859-1" || e == "latin1" || e == "latin-1" => Ok(Decoded {
            text: bytes.iter().map(|&b| char::from(b)).collect(),
            encoding: Encoding::Latin1,
            had_bom: false,
        }),
        Some(_) => Err(DecodeError::UnsupportedEncoding(
            declared_encoding(bytes).unwrap_or_default(),
        )),
    }
}

/// Encodes text for writing a request file: UTF-8, with a BOM only if asked to preserve one.
pub fn encode_utf8(text: &str, with_bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 3);
    if with_bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

fn utf8(bytes: &[u8], had_bom: bool) -> Result<Decoded, DecodeError> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(Decoded {
            text: s.to_owned(),
            encoding: Encoding::Utf8,
            had_bom,
        }),
        Err(e) => Err(DecodeError::Invalid {
            encoding: Encoding::Utf8,
            offset: e.valid_up_to() + usize::from(had_bom) * 3,
        }),
    }
}

fn utf16(bytes: &[u8], encoding: Encoding, had_bom: bool) -> Result<Decoded, DecodeError> {
    let bom_len = if had_bom { 2 } else { 0 };
    if !bytes.len().is_multiple_of(2) {
        return Err(DecodeError::Invalid {
            encoding,
            offset: bom_len + bytes.len() - 1,
        });
    }
    let units = bytes.chunks_exact(2).map(|c| match encoding {
        Encoding::Utf16Be => u16::from_be_bytes([c[0], c[1]]),
        _ => u16::from_le_bytes([c[0], c[1]]),
    });
    let mut text = String::with_capacity(bytes.len() / 2);
    for (i, r) in char::decode_utf16(units).enumerate() {
        match r {
            Ok(ch) => text.push(ch),
            Err(_) => {
                return Err(DecodeError::Invalid {
                    encoding,
                    offset: bom_len + i * 2,
                });
            }
        }
    }
    Ok(Decoded {
        text,
        encoding,
        had_bom,
    })
}

/// Reads `encoding="…"` from an ASCII-compatible XML declaration, if present.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.iter().position(|&b| b == b'>')?;
    let decl = std::str::from_utf8(&bytes[..end]).ok()?;
    let at = decl.find("encoding")?;
    let rest = decl[at + "encoding".len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &rest[1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_is_stripped() {
        let d = decode(b"\xEF\xBB\xBF<a/>").expect("decodes");
        assert_eq!(d.text, "<a/>");
        assert!(d.had_bom);
        assert_eq!(d.encoding, Encoding::Utf8);
    }

    #[test]
    fn plain_utf8() {
        let d = decode("<?xml version=\"1.0\"?><a>ü</a>".as_bytes()).expect("decodes");
        assert_eq!(d.text, "<?xml version=\"1.0\"?><a>ü</a>");
        assert!(!d.had_bom);
    }

    #[test]
    fn utf16le_with_bom() {
        let mut b = vec![0xFF, 0xFE];
        for u in "<a>ü</a>".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode(&b).expect("decodes");
        assert_eq!(d.text, "<a>ü</a>");
        assert_eq!(d.encoding, Encoding::Utf16Le);
    }

    #[test]
    fn utf16be_without_bom() {
        let mut b = vec![];
        for u in "<?xml version='1.0' encoding='UTF-16'?><a/>".encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        let d = decode(&b).expect("decodes");
        assert_eq!(d.encoding, Encoding::Utf16Be);
        assert!(d.text.ends_with("<a/>"));
    }

    #[test]
    fn latin1_from_declaration() {
        let d = decode(b"<?xml version='1.0' encoding='ISO-8859-1'?><a>\xFC</a>").expect("decodes");
        assert_eq!(d.encoding, Encoding::Latin1);
        assert!(d.text.ends_with("<a>ü</a>"));
    }

    #[test]
    fn unknown_encoding_is_rejected() {
        let e = decode(b"<?xml version='1.0' encoding='EBCDIC-1'?><a/>").expect_err("rejects");
        assert_eq!(e, DecodeError::UnsupportedEncoding("EBCDIC-1".into()));
    }

    #[test]
    fn invalid_utf8_reports_offset_including_bom() {
        let e = decode(b"\xEF\xBB\xBF<a>\xFF</a>").expect_err("rejects");
        assert_eq!(
            e,
            DecodeError::Invalid {
                encoding: Encoding::Utf8,
                offset: 6
            }
        );
    }
}
