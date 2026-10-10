//! Request and folder names: validation and automatic naming.
//!
//! A request name is the file stem of `requests/<folder>/<name>.xml`; a folder is a
//! subdirectory of `requests/`, nested to any depth, and its path is `/`-separated whatever the
//! platform. Clashes are detected case-insensitively because the default macOS file system is
//! case-insensitive: `GetCustomer 1` and `getcustomer 1` would be the same file there.

use std::collections::HashSet;

use thiserror::Error;

/// Request file extension, including the dot.
pub(crate) const REQUEST_EXT: &str = ".xml";

/// Most file systems limit a file name to 255 bytes; the stem leaves room for `.xml`.
const MAX_NAME_BYTES: usize = 255 - REQUEST_EXT.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("the name is empty")]
    Empty,
    #[error("the name may not contain '/'")]
    Slash,
    #[error("the name may not contain ':'")]
    Colon,
    #[error("the name may not start with '.'")]
    LeadingDot,
    #[error("the name may not contain control characters")]
    ControlChar,
    #[error("the name is too long")]
    TooLong,
}

/// Checks a name chosen by the user (or generated from an operation name).
pub fn validate_request_name(name: &str) -> Result<(), NameError> {
    if name.trim().is_empty() {
        return Err(NameError::Empty);
    }
    if name.contains('/') {
        return Err(NameError::Slash);
    }
    // ':' is the path separator in the Carbon/Finder view of HFS+/APFS names.
    if name.contains(':') {
        return Err(NameError::Colon);
    }
    if name.starts_with('.') {
        return Err(NameError::LeadingDot);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlChar);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    Ok(())
}

/// Checks a folder path relative to `requests/`: `""` is the top level, otherwise every
/// `/`-separated component follows the request name rules.
pub fn validate_folder_path(path: &str) -> Result<(), NameError> {
    if path.is_empty() {
        return Ok(());
    }
    path.split('/').try_for_each(validate_request_name)
}

/// `folder/name`, or `name` at the top level.
pub(crate) fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// The folder part and the last component of a `/`-separated path; the folder is `""` at
/// the top level.
pub(crate) fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Whether `path` is `folder` or inside it. Every path is inside the top level, `""`.
pub(crate) fn is_within(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Key for clash detection.
pub(crate) fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// The request name for a file name in `requests/`, or `None` if the file is not a request
/// (hidden or temporary files, other extensions, invalid names).
pub(crate) fn stem_of(file_name: &str) -> Option<&str> {
    if file_name.starts_with('.') || file_name.len() <= REQUEST_EXT.len() {
        return None;
    }
    let split = file_name.len() - REQUEST_EXT.len();
    let (stem, ext) = (file_name.get(..split)?, file_name.get(split..)?);
    ext.eq_ignore_ascii_case(REQUEST_EXT).then_some(stem)
}

/// `<base> 1`, `<base> 2`, … — the lowest number not in `taken` (folded names).
pub(crate) fn numbered(base: &str, taken: &HashSet<String>) -> String {
    (1u64..)
        .map(|n| format!("{base} {n}"))
        .find(|c| !taken.contains(&fold(c)))
        .unwrap_or_else(|| base.to_owned())
}

/// `<name> copy`, then `<name> copy 2`, `<name> copy 3`, …
pub(crate) fn copy_name(name: &str, taken: &HashSet<String>) -> String {
    let first = format!("{name} copy");
    if !taken.contains(&fold(&first)) {
        return first;
    }
    (2u64..)
        .map(|n| format!("{name} copy {n}"))
        .find(|c| !taken.contains(&fold(c)))
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| fold(n)).collect()
    }

    #[test]
    fn validation() {
        assert_eq!(validate_request_name("GetCustomer 1"), Ok(()));
        assert_eq!(validate_request_name("Grüße – ok"), Ok(()));
        assert_eq!(validate_request_name(""), Err(NameError::Empty));
        assert_eq!(validate_request_name("  "), Err(NameError::Empty));
        assert_eq!(validate_request_name("a/b"), Err(NameError::Slash));
        assert_eq!(validate_request_name("a:b"), Err(NameError::Colon));
        assert_eq!(validate_request_name(".hidden"), Err(NameError::LeadingDot));
        assert_eq!(validate_request_name("a\tb"), Err(NameError::ControlChar));
        assert_eq!(
            validate_request_name(&"x".repeat(252)),
            Err(NameError::TooLong)
        );
        assert_eq!(validate_request_name(&"x".repeat(251)), Ok(()));
    }

    #[test]
    fn folder_paths() {
        assert_eq!(validate_folder_path(""), Ok(()));
        assert_eq!(validate_folder_path("Port/GetCustomer"), Ok(()));
        assert_eq!(validate_folder_path("a//b"), Err(NameError::Empty));
        assert_eq!(validate_folder_path("/a"), Err(NameError::Empty));
        assert_eq!(validate_folder_path("a/.git"), Err(NameError::LeadingDot));
        assert_eq!(validate_folder_path("a/b:c"), Err(NameError::Colon));
    }

    #[test]
    fn paths() {
        assert_eq!(join("", "A"), "A");
        assert_eq!(join("F/G", "A"), "F/G/A");
        assert_eq!(split("A"), ("", "A"));
        assert_eq!(split("F/G/A"), ("F/G", "A"));
        assert!(is_within("F/G", "F"));
        assert!(is_within("F", "F"));
        assert!(is_within("F", ""));
        assert!(!is_within("FG", "F"));
        assert!(!is_within("F", "F/G"));
    }

    #[test]
    fn stems() {
        assert_eq!(stem_of("GetCustomer 1.xml"), Some("GetCustomer 1"));
        assert_eq!(stem_of("Upper.XML"), Some("Upper"));
        assert_eq!(stem_of(".wb-123.tmp"), None);
        assert_eq!(stem_of(".xml"), None);
        assert_eq!(stem_of("notes.txt"), None);
        assert_eq!(stem_of("ü.xml"), Some("ü"));
    }

    #[test]
    fn lowest_free_number() {
        assert_eq!(numbered("Op", &set(&[])), "Op 1");
        assert_eq!(numbered("Op", &set(&["op 1", "Op 3"])), "Op 2");
    }

    #[test]
    fn copies() {
        assert_eq!(copy_name("A", &set(&["A"])), "A copy");
        assert_eq!(copy_name("A", &set(&["A", "A copy"])), "A copy 2");
        assert_eq!(
            copy_name("A", &set(&["A", "a COPY", "A copy 2"])),
            "A copy 3"
        );
    }
}
