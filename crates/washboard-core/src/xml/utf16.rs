//! Byte offset ↔ UTF-16 offset conversion.
//!
//! Everything in [`crate::xml`] works with byte offsets into a `&str`; AppKit (`NSString`,
//! `NSRange`, `NSTextStorage`) counts UTF-16 code units. Characters outside the BMP are one
//! `char`, four UTF-8 bytes and two UTF-16 units, so the two never agree on such text.
//!
//! All functions clamp out-of-range input and snap offsets that fall inside a character (or
//! between the two halves of a surrogate pair) back to the start of that character.

/// UTF-16 units contributed by a UTF-8 byte: leads count for the whole char, continuations 0.
fn units(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0x80..=0xBF => 0,
        0xC0..=0xEF => 1,
        _ => 2,
    }
}

/// Length of `s` in UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.bytes().map(units).sum()
}

/// UTF-16 offset of byte offset `byte` in `text`.
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    let byte = text.floor_char_boundary(byte);
    utf16_len(&text[..byte])
}

/// Byte offset of UTF-16 offset `utf16` in `text`.
pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut seen = 0;
    for (i, ch) in text.char_indices() {
        let next = seen + ch.len_utf16();
        if next > utf16 {
            return i;
        }
        seen = next;
    }
    text.len()
}

/// Converts many offsets of one text, cheaply when they come in ascending order.
///
/// Converting a token list one offset at a time with [`byte_to_utf16`] is quadratic on a large
/// document; this keeps its place, so ascending queries cost O(distance moved). A query
/// behind the current place restarts from the beginning (still correct, just slower).
#[derive(Debug, Clone)]
pub struct Utf16Cursor<'a> {
    text: &'a str,
    byte: usize,
    utf16: usize,
}

impl<'a> Utf16Cursor<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            byte: 0,
            utf16: 0,
        }
    }

    /// UTF-16 offset of byte offset `byte`.
    pub fn utf16_at(&mut self, byte: usize) -> usize {
        let byte = self.text.floor_char_boundary(byte);
        if byte < self.byte {
            self.byte = 0;
            self.utf16 = 0;
        }
        self.utf16 += utf16_len(&self.text[self.byte..byte]);
        self.byte = byte;
        self.utf16
    }

    /// Byte offset of UTF-16 offset `utf16`.
    pub fn byte_at(&mut self, utf16: usize) -> usize {
        if utf16 < self.utf16 {
            self.byte = 0;
            self.utf16 = 0;
        }
        for ch in self.text[self.byte..].chars() {
            let next = self.utf16 + ch.len_utf16();
            if next > utf16 {
                return self.byte;
            }
            self.utf16 = next;
            self.byte += ch.len_utf8();
        }
        self.byte
    }

    /// UTF-16 range of a byte range, e.g. a [`super::Token`] span for `NSRange`.
    pub fn utf16_range(&mut self, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let start = self.utf16_at(range.start);
        let end = self.utf16_at(range.end.max(range.start));
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_widths() {
        // a(1B,1U) ü(2B,1U) €(3B,1U) 😀(4B,2U) b
        let t = "aü€😀b";
        assert_eq!(utf16_len(t), 6);
        assert_eq!(byte_to_utf16(t, 0), 0);
        assert_eq!(byte_to_utf16(t, 1), 1);
        assert_eq!(byte_to_utf16(t, 3), 2);
        assert_eq!(byte_to_utf16(t, 6), 3);
        assert_eq!(byte_to_utf16(t, 10), 5);
        assert_eq!(byte_to_utf16(t, 11), 6);
        assert_eq!(byte_to_utf16(t, 99), 6);
        // inside 😀 snaps to its start
        assert_eq!(byte_to_utf16(t, 8), 3);
        assert_eq!(utf16_to_byte(t, 3), 6);
        assert_eq!(utf16_to_byte(t, 4), 6, "between surrogates snaps back");
        assert_eq!(utf16_to_byte(t, 5), 10);
        assert_eq!(utf16_to_byte(t, 6), 11);
        assert_eq!(utf16_to_byte(t, 60), 11);
    }

    #[test]
    fn cursor_matches_free_functions() {
        let t = "<a x='😀'>ü€ text 😀😀</a>";
        let mut c = Utf16Cursor::new(t);
        for b in (0..=t.len() + 2).chain([3, 0, 7]) {
            assert_eq!(c.utf16_at(b), byte_to_utf16(t, b), "byte {b}");
        }
        let mut c = Utf16Cursor::new(t);
        for u in (0..=utf16_len(t) + 2).chain([2, 0, 9]) {
            assert_eq!(c.byte_at(u), utf16_to_byte(t, u), "utf16 {u}");
        }
    }
}
