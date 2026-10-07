//! Input file sets, path arithmetic and matching of import locations to supplied files.
//!
//! Paths are handled as `/`-separated strings, normalized lexically (no symlink resolution,
//! no file system access), so the same code serves files on disk and files held in memory.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// One supplied WSDL or XSD file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// `/`-separated path. Absolute for files loaded from disk; for in-memory sets any
    /// consistent relative layout works (e.g. paths relative to a project's `wsdl/` folder).
    /// Relative `schemaLocation`s are resolved against this path.
    pub path: String,
    /// The original bytes; decoded via [`crate::xml::decode`] when read.
    pub bytes: Vec<u8>,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }
}

/// The entry WSDL plus every file the user supplied for resolving references.
///
/// Nothing outside this set is ever read: references are matched against it or reported
/// as unresolved (`docs/PLAN.md` §4 "Create project").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    files: Vec<SourceFile>,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("more than {limit} files found under {dir}")]
    TooManyFiles { dir: PathBuf, limit: usize },
}

/// Upper bound for files collected from directories; protects against picking `/` by mistake.
pub const MAX_DIR_FILES: usize = 10_000;
const MAX_DIR_DEPTH: usize = 32;

impl Sources {
    /// Builds a set from in-memory files. Paths are normalized; when two files normalize to
    /// the same path the first one wins.
    pub fn new(entry: SourceFile, others: impl IntoIterator<Item = SourceFile>) -> Self {
        let mut files: Vec<SourceFile> = Vec::new();
        let mut seen = HashMap::new();
        for mut f in std::iter::once(entry).chain(others) {
            f.path = normalize(&f.path);
            if seen.insert(f.path.clone(), ()).is_none() {
                files.push(f);
            }
        }
        Self { files }
    }

    /// Reads the entry WSDL and the extra files from disk. Each extra path may be a file or a
    /// directory; directories are searched recursively for `*.wsdl` and `*.xsd`, skipping
    /// hidden entries (so a project's `wsdl/.previous/` is never picked up) and symlinked
    /// directories.
    pub fn from_disk(entry: &Path, extra: &[PathBuf]) -> Result<Self, SourceError> {
        let entry_file = read_file(entry)?;
        let mut others = Vec::new();
        for p in extra {
            let meta = fs::metadata(p).map_err(|source| SourceError::Io {
                path: p.clone(),
                source,
            })?;
            if meta.is_dir() {
                let mut found = Vec::new();
                collect_dir(p, p, 0, &mut found)?;
                found.sort();
                for f in found {
                    others.push(read_file(&f)?);
                }
            } else {
                others.push(read_file(p)?);
            }
        }
        Ok(Self::new(entry_file, others))
    }

    /// The entry WSDL is always the first file.
    pub fn entry(&self) -> &SourceFile {
        &self.files[0]
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }
}

fn read_file(path: &Path) -> Result<SourceFile, SourceError> {
    let abs = std::path::absolute(path).map_err(|source| SourceError::Io {
        path: path.to_owned(),
        source,
    })?;
    let s = abs
        .to_str()
        .ok_or_else(|| SourceError::NonUtf8Path(abs.clone()))?
        .to_owned();
    let bytes = fs::read(&abs).map_err(|source| SourceError::Io {
        path: abs.clone(),
        source,
    })?;
    Ok(SourceFile::new(s, bytes))
}

fn collect_dir(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), SourceError> {
    if depth > MAX_DIR_DEPTH {
        return Ok(());
    }
    let io = |source| SourceError::Io {
        path: dir.to_owned(),
        source,
    };
    for entry in fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let ft = entry.file_type().map_err(io)?;
        let path = entry.path();
        if ft.is_dir() {
            collect_dir(root, &path, depth + 1, out)?;
        } else if ft.is_file() {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("wsdl" | "xsd")) {
                if out.len() >= MAX_DIR_FILES {
                    return Err(SourceError::TooManyFiles {
                        dir: root.to_owned(),
                        limit: MAX_DIR_FILES,
                    });
                }
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Lexically normalizes a `/`-separated path: removes `.` and empty segments, folds `..`.
/// Leading `..` of relative paths are kept; `..` above `/` is dropped.
pub(crate) fn normalize(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for s in path.split('/') {
        match s {
            "" | "." => {}
            ".." => match segs.last() {
                Some(&l) if l != ".." => {
                    segs.pop();
                }
                _ if abs => {}
                _ => segs.push(".."),
            },
            s => segs.push(s),
        }
    }
    let joined = segs.join("/");
    if abs { format!("/{joined}") } else { joined }
}

/// Directory part of a normalized path (`""` for a bare file name).
pub(crate) fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

/// Resolves `rel` against the directory `dir`.
pub(crate) fn join(dir: &str, rel: &str) -> String {
    if rel.starts_with('/') || dir.is_empty() {
        normalize(rel)
    } else {
        normalize(&format!("{dir}/{rel}"))
    }
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Decodes `%XX` escapes; invalid escapes and non-UTF-8 results are kept verbatim.
pub(crate) fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| (*c as char).to_digit(16)),
                b.get(i + 2).and_then(|c| (*c as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// Percent-encodes everything but RFC 3986 unreserved characters and `/`, so the result is
/// a canonical URI path that libxml2's URI handling leaves unchanged.
pub(crate) fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A `schemaLocation` / `location` value, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    /// A path, relative to the importing file or absolute (`/…`, `file:` URLs).
    Path(String),
    /// A URL with a scheme other than `file:`; never fetched, matched by path suffix.
    Remote {
        segments: Vec<String>,
        has_query: bool,
    },
}

pub(crate) fn classify(raw: &str) -> Location {
    let s = raw.trim();
    let s = s.split('#').next().unwrap_or("");
    if let Some(colon) = s.find(':') {
        let scheme = &s[..colon];
        let is_scheme = scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if is_scheme {
            let rest = &s[colon + 1..];
            if scheme.eq_ignore_ascii_case("file") {
                let path = match rest.strip_prefix("//") {
                    Some(after) => after.find('/').map_or("", |i| &after[i..]),
                    None => rest,
                };
                return Location::Path(normalize(&percent_decode(path)));
            }
            let after_authority = match rest.strip_prefix("//") {
                Some(after) => after.find('/').map_or("", |i| &after[i..]),
                None => rest,
            };
            let (path, query) = match after_authority.split_once('?') {
                Some((p, q)) => (p, Some(q)),
                None => (after_authority, None),
            };
            return Location::Remote {
                segments: segments(&normalize(&percent_decode(path)))
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                has_query: query.is_some_and(|q| !q.is_empty()),
            };
        }
    }
    let path = s.split('?').next().unwrap_or("");
    Location::Path(percent_decode(path))
}

/// How a reference was matched to a supplied file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchedBy {
    /// Relative or absolute path, resolved against the importing file. The normal case.
    Path,
    /// Path matched only when ignoring case (works on default macOS volumes, not elsewhere).
    PathIgnoringCase,
    /// The path did not exist among the supplied files; matched by trailing path segments.
    PathSuffix { segments: usize },
    /// Remote URL (never fetched), matched by the longest trailing path segment match.
    UrlSuffix { segments: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Match {
    Found(usize, MatchedBy),
    Ambiguous(Vec<usize>),
    NotFound,
}

/// Index over supplied file paths for resolving references.
#[derive(Debug)]
pub(crate) struct FileIndex {
    exact: HashMap<String, usize>,
    lower: HashMap<String, Vec<usize>>,
    segs: Vec<Vec<String>>,
}

impl FileIndex {
    pub(crate) fn new(files: &[SourceFile]) -> Self {
        let mut exact = HashMap::new();
        let mut lower: HashMap<String, Vec<usize>> = HashMap::new();
        let mut segs = Vec::new();
        for (i, f) in files.iter().enumerate() {
            exact.insert(f.path.clone(), i);
            lower.entry(f.path.to_lowercase()).or_default().push(i);
            segs.push(segments(&f.path).into_iter().map(str::to_owned).collect());
        }
        Self { exact, lower, segs }
    }

    /// Resolves `loc` as written in the file at `importer`.
    pub(crate) fn resolve(&self, importer: &str, loc: &Location) -> Match {
        match loc {
            Location::Path(p) => {
                let full = join(dir_of(importer), p);
                if let Some(&i) = self.exact.get(&full) {
                    return Match::Found(i, MatchedBy::Path);
                }
                if let Some(v) = self.lower.get(&full.to_lowercase()) {
                    return match v.as_slice() {
                        [i] => Match::Found(*i, MatchedBy::PathIgnoringCase),
                        _ => Match::Ambiguous(v.clone()),
                    };
                }
                let want: Vec<&str> = segments(&full).into_iter().filter(|s| *s != "..").collect();
                // Never "resolve" a file to itself by name only.
                let skip = self.exact.get(importer).copied();
                match self.suffix(&want, skip) {
                    Match::Found(i, MatchedBy::UrlSuffix { segments }) => {
                        Match::Found(i, MatchedBy::PathSuffix { segments })
                    }
                    m => m,
                }
            }
            Location::Remote { segments, .. } => {
                let want: Vec<&str> = segments.iter().map(String::as_str).collect();
                self.suffix(&want, None)
            }
        }
    }

    fn suffix(&self, want: &[&str], skip: Option<usize>) -> Match {
        if want.is_empty() {
            return Match::NotFound;
        }
        let mut best = 0;
        let mut found: Vec<usize> = Vec::new();
        for (i, have) in self.segs.iter().enumerate() {
            let k = have
                .iter()
                .rev()
                .zip(want.iter().rev())
                .take_while(|(a, b)| a.as_str() == **b)
                .count();
            if k == 0 || k < best || Some(i) == skip {
                continue;
            }
            if k > best {
                best = k;
                found.clear();
            }
            found.push(i);
        }
        match found.as_slice() {
            [] => Match::NotFound,
            [i] => Match::Found(*i, MatchedBy::UrlSuffix { segments: best }),
            _ => Match::Ambiguous(found),
        }
    }
}

/// Where a supplied file goes inside the project's `wsdl/` folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEntry {
    /// [`SourceFile::path`] as supplied.
    pub source: String,
    /// `/`-separated path relative to the project's `wsdl/` folder.
    pub dest: String,
}

/// Destinations relative to the deepest directory containing all supplied files, so
/// relative references between them keep working after the copy.
pub(crate) fn layout(files: &[SourceFile]) -> Vec<LayoutEntry> {
    let dirs: Vec<Vec<&str>> = files.iter().map(|f| segments(dir_of(&f.path))).collect();
    let mut common = dirs.first().map_or(0, Vec::len);
    for d in &dirs {
        common = common.min(
            d.iter()
                .zip(dirs[0].iter())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    files
        .iter()
        .map(|f| {
            let segs = segments(&f.path);
            // Leading ".." segments of relative in-memory paths cannot be expressed inside
            // the project folder; they are kept as plain names.
            let dest = segs
                .get(common..)
                .unwrap_or(&[])
                .iter()
                .map(|s| if *s == ".." { "_up" } else { s })
                .collect::<Vec<_>>()
                .join("/");
            LayoutEntry {
                source: f.path.clone(),
                dest,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("/a/./b/../c//d.xsd"), "/a/c/d.xsd");
        assert_eq!(normalize("../x/../../y.xsd"), "../../y.xsd");
        assert_eq!(normalize("/../y.xsd"), "/y.xsd");
        assert_eq!(join("/a/b", "../c.xsd"), "/a/c.xsd");
        assert_eq!(join("", "c.xsd"), "c.xsd");
        assert_eq!(dir_of("/a.wsdl"), "/");
        assert_eq!(dir_of("a.wsdl"), "");
    }

    #[test]
    fn classifies_locations() {
        assert_eq!(
            classify(" xsd/a%20b.xsd "),
            Location::Path("xsd/a b.xsd".into())
        );
        assert_eq!(
            classify("file:///tmp/x.xsd"),
            Location::Path("/tmp/x.xsd".into())
        );
        assert_eq!(
            classify("https://h.example/a/b/c.xsd?x=1#f"),
            Location::Remote {
                segments: vec!["a".into(), "b".into(), "c.xsd".into()],
                has_query: true
            }
        );
        // A Windows drive letter is not a scheme.
        assert_eq!(classify("C:/x.xsd"), Location::Path("C:/x.xsd".into()));
    }

    #[test]
    fn matches_by_path_case_and_suffix() {
        let files = [
            SourceFile::new("/p/svc.wsdl", ""),
            SourceFile::new("/p/xsd/common.xsd", ""),
            SourceFile::new("/p/v1/common/types.xsd", ""),
            SourceFile::new("/p/v2/common/types.xsd", ""),
            SourceFile::new("/q/v2/other/Types.xsd", ""),
        ];
        let ix = FileIndex::new(&files);
        let r = |loc: &str| ix.resolve("/p/svc.wsdl", &classify(loc));
        assert_eq!(r("xsd/common.xsd"), Match::Found(1, MatchedBy::Path));
        assert_eq!(
            r("XSD/Common.xsd"),
            Match::Found(1, MatchedBy::PathIgnoringCase)
        );
        assert_eq!(
            r("http://h/schemas/v2/common/types.xsd"),
            Match::Found(3, MatchedBy::UrlSuffix { segments: 3 })
        );
        assert_eq!(r("http://h/x/types.xsd"), Match::Ambiguous(vec![2, 3]));
        assert_eq!(r("http://h/x/none.xsd"), Match::NotFound);
        assert_eq!(
            r("elsewhere/common.xsd"),
            Match::Found(1, MatchedBy::PathSuffix { segments: 1 })
        );
    }

    #[test]
    fn layout_keeps_relative_structure() {
        let files = [
            SourceFile::new("/home/u/svc/api/Svc.wsdl", ""),
            SourceFile::new("/home/u/svc/xsd/a.xsd", ""),
        ];
        let l = layout(&files);
        assert_eq!(l[0].dest, "api/Svc.wsdl");
        assert_eq!(l[1].dest, "xsd/a.xsd");
        let single = layout(&[SourceFile::new("/x/S.wsdl", "")]);
        assert_eq!(single[0].dest, "S.wsdl");
    }

    #[test]
    fn percent_encoding_round_trips() {
        assert_eq!(percent_encode_path("a b/ü.xsd"), "a%20b/%C3%BC.xsd");
        assert_eq!(percent_decode("a%20b/%C3%BC.xsd"), "a b/ü.xsd");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }
}
