//! Loading from disk with the referenced files that sit where the references say.
//!
//! A WSDL usually ships with its XSDs beside it and imports them by relative path. Making the
//! user add each of them by hand is busywork, so [`load_from_disk`] follows unresolved
//! relative references to files that exist on disk at the referenced path, adds them, and
//! loads again until nothing new turns up. Only local files named by a relative path are
//! read; remote locations are still never fetched (PLAN §4, §6).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::source::{Location, classify, dir_of, join, read_file};
use super::{Resolution, SourceError, SourceFile, Sources, Unresolved, Wsdl, load};

/// What [`load_from_disk`] read.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub sources: Sources,
    pub wsdl: Wsdl,
    /// Files read because a reference named them, not because the caller supplied them; in
    /// the order they were found.
    pub found: Vec<PathBuf>,
}

/// [`Sources::from_disk`] and [`load`], plus the files that unresolved relative references
/// name and that exist on disk relative to the importing file, transitively.
pub fn load_from_disk(entry: &Path, extra: &[PathBuf]) -> Result<Loaded, SourceError> {
    let base = Sources::from_disk(entry, extra)?;
    let mut found: Vec<PathBuf> = Vec::new();
    let mut found_files: Vec<SourceFile> = Vec::new();
    // Every path tried, so a file that exists but resolves nothing is not read again.
    let mut tried: HashSet<String> = HashSet::new();
    loop {
        let sources = with_found(&base, &found_files);
        let wsdl = load(&sources);
        let mut new = false;
        for path in candidates(&sources, &wsdl) {
            if !tried.insert(path.clone()) {
                continue;
            }
            let on_disk = PathBuf::from(&path);
            if !on_disk.is_file() {
                continue;
            }
            found_files.push(read_file(&on_disk)?);
            found.push(on_disk);
            new = true;
        }
        if !new {
            return Ok(Loaded {
                sources,
                wsdl,
                found,
            });
        }
    }
}

fn with_found(base: &Sources, found: &[SourceFile]) -> Sources {
    let mut files = base.files().iter().cloned();
    let entry = files.next().expect("a source set has its entry");
    Sources::new(entry, files.chain(found.iter().cloned()))
}

/// Absolute paths that unresolved, not-supplied relative references point at.
fn candidates(sources: &Sources, wsdl: &Wsdl) -> Vec<String> {
    let files = sources.files();
    wsdl.check
        .references
        .iter()
        .filter(|r| r.resolution == Resolution::Unresolved(Unresolved::NotSupplied))
        .filter_map(|r| {
            let Location::Path(path) = classify(r.location.as_deref()?) else {
                return None;
            };
            // Only relative paths: "next to the importing file", not anywhere on the disk.
            if path.starts_with('/') {
                return None;
            }
            let importer = wsdl.check.files.iter().position(|f| f.path == r.from)?;
            let importer = &files.get(importer)?.path;
            // Only files loaded from disk have absolute paths to resolve against.
            importer
                .starts_with('/')
                .then(|| join(dir_of(importer), &path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;
    use crate::soap::{WSDL_NS, XSD_NS};

    fn wsdl_importing(location: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<wsdl:definitions xmlns:wsdl="{WSDL_NS}" xmlns:xs="{XSD_NS}" targetNamespace="urn:t">
  <wsdl:types>
    <xs:schema targetNamespace="urn:t">
      <xs:import namespace="urn:a" schemaLocation="{location}"/>
    </xs:schema>
  </wsdl:types>
</wsdl:definitions>
"#
        )
    }

    fn xsd(tns: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="{XSD_NS}" targetNamespace="{tns}">{body}</xs:schema>
"#
        )
    }

    #[test]
    fn finds_relative_imports_next_to_the_importer_transitively() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("types")).unwrap();
        fs::write(root.join("S.wsdl"), wsdl_importing("types/a.xsd")).unwrap();
        // a.xsd imports b.xsd relative to itself, in the same folder.
        let import_b = r#"<xs:import namespace="urn:b" schemaLocation="b.xsd"/>"#;
        fs::write(root.join("types/a.xsd"), xsd("urn:a", import_b)).unwrap();
        fs::write(root.join("types/b.xsd"), xsd("urn:b", "")).unwrap();
        fs::write(root.join("unrelated.xsd"), xsd("urn:u", "")).unwrap();

        let loaded = load_from_disk(&root.join("S.wsdl"), &[]).unwrap();
        let names: Vec<&str> = loaded
            .found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["a.xsd", "b.xsd"], "only what is referenced");
        assert!(!loaded.wsdl.check.has_errors(), "{:?}", loaded.wsdl.check);
        assert_eq!(loaded.sources.files().len(), 3);
    }

    #[test]
    fn supplied_files_win_and_missing_ones_stay_unresolved() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("S.wsdl"), wsdl_importing("a.xsd")).unwrap();
        fs::write(root.join("a.xsd"), xsd("urn:a", "")).unwrap();
        let loaded = load_from_disk(&root.join("S.wsdl"), &[root.join("a.xsd")]).unwrap();
        assert!(loaded.found.is_empty(), "already supplied");
        assert!(!loaded.wsdl.check.has_errors());

        fs::write(root.join("T.wsdl"), wsdl_importing("missing.xsd")).unwrap();
        let loaded = load_from_disk(&root.join("T.wsdl"), &[]).unwrap();
        assert!(loaded.found.is_empty());
        assert!(
            loaded.wsdl.check.has_errors(),
            "still reported as not supplied"
        );
    }

    #[test]
    fn remote_and_absolute_locations_are_not_followed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.xsd"), xsd("urn:a", "")).unwrap();
        let absolute = root.join("a.xsd").display().to_string();
        for location in ["https://example.com/a.xsd", absolute.as_str()] {
            fs::write(root.join("S.wsdl"), wsdl_importing(location)).unwrap();
            let loaded = load_from_disk(&root.join("S.wsdl"), &[]).unwrap();
            assert!(loaded.found.is_empty(), "{location} followed");
        }
    }
}
