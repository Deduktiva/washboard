//! Text helpers of the app that need no AppKit, so their tests run on Linux too.

use std::ops::Range;

/// The byte range of `old` that was replaced, and the length of its replacement in `new`.
/// Any consistent description works for `TokenBuffer::edit`; this one is the smallest.
pub(crate) fn changed_range(old: &str, new: &str) -> (Range<usize>, usize) {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    // Keep both ends on char boundaries.
    let mut start = prefix;
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut old_end = old.len() - suffix;
    let mut new_end = new.len() - suffix;
    while !old.is_char_boundary(old_end) || !new.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    (start..old_end, new_end - start)
}

/// The edit that turns `old` into `new`: the UTF-16 range of `old` to replace and the text to
/// put there. For when the text view changed without saying how.
pub(crate) fn utf16_edit(old: &str, new: &str) -> (Range<usize>, String) {
    let (bytes, new_len) = changed_range(old, new);
    let mut cursor = washboard_core::xml::utf16::Utf16Cursor::new(old);
    let range = cursor.utf16_range(bytes.clone());
    (range, new[bytes.start..bytes.start + new_len].to_owned())
}

/// The request text with every `Authorization` header value replaced by bullets.
pub(crate) fn mask_authorization(request: &str) -> String {
    request
        .split_inclusive('\n')
        .map(|line| match line.split_once(':') {
            Some((name, value)) if name.eq_ignore_ascii_case("authorization") => {
                let end = if value.ends_with('\n') { "\n" } else { "" };
                format!("{name}: ••••••••{end}")
            }
            _ => line.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{changed_range, mask_authorization, utf16_edit};

    #[test]
    fn utf16_edit_counts_utf16_units() {
        assert_eq!(utf16_edit("<a>ä</a>", "<a>äb</a>"), (4..4, "b".into()));
        assert_eq!(utf16_edit("😀x", "😀"), (2..3, String::new()));
        assert_eq!(utf16_edit("abc", "abc"), (3..3, String::new()));
    }

    #[test]
    fn changed_range_finds_the_edit() {
        assert_eq!(changed_range("<a></a>", "<ab></a>"), (2..2, 1));
        assert_eq!(changed_range("<ab></a>", "<a></a>"), (2..3, 0));
        assert_eq!(changed_range("abc", "abc"), (3..3, 0));
        assert_eq!(changed_range("", "x"), (0..0, 1));
        // Repeated characters: any consistent answer will do, this one is the smallest.
        assert_eq!(changed_range("aaa", "aaaa"), (3..3, 1));
        // Multi-byte chars stay whole: é and è share their first UTF-8 byte.
        assert_eq!(changed_range("é", "è"), (0..2, 2));
    }

    #[test]
    fn masks_only_authorization_values() {
        let masked = mask_authorization("POST / HTTP/1.1\nauthorization: Basic abc\nHost: x\n");
        assert_eq!(
            masked,
            "POST / HTTP/1.1\nauthorization: ••••••••\nHost: x\n"
        );
    }
}
