//! Finding what an edit changed, for incremental highlighting. Platform-independent, so its
//! tests run on Linux too.

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

#[cfg(test)]
mod tests {
    use super::changed_range;

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
}
