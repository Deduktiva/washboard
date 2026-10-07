//! File-system helpers: atomic writes, the folder lock, error context.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::ProjectError;

/// Attaches the path to an I/O error.
pub(crate) trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T, ProjectError>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T, ProjectError> {
        self.map_err(|source| ProjectError::Io {
            path: path.to_owned(),
            source,
        })
    }
}

/// Writes `bytes` to `path` via a temp file in the same directory and `rename`, so readers
/// (and a crash) see either the old or the new content, never a mix.
///
/// The temp name starts with `.` so request reconciliation ignores leftovers; `tempfile` removes
/// the temp file if anything fails before the rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".wb-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .at(path)?;
    tmp.write_all(bytes).at(path)?;
    tmp.as_file().sync_all().at(path)?;
    tmp.persist(path).map_err(|e| e.error).at(path)?;
    // Persist the rename itself. Best effort: not every file system allows fsync on a directory.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Removes a file; a file that is already gone is fine.
pub(crate) fn remove_file_if_exists(path: &Path) -> Result<(), ProjectError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(ProjectError::Io {
            path: path.to_owned(),
            source: e,
        }),
        _ => Ok(()),
    }
}

/// Removes a directory tree; a directory that is already gone is fine.
pub(crate) fn remove_dir_if_exists(path: &Path) -> Result<(), ProjectError> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(ProjectError::Io {
            path: path.to_owned(),
            source: e,
        }),
        _ => Ok(()),
    }
}

/// Takes the exclusive `flock` that marks a project folder as open.
///
/// The lock is on a separate file, not on `washboard.sqlite`: on macOS (BSD) `flock` and the
/// `fcntl` locks SQLite takes on its database file interact, so locking the database itself
/// would make SQLite's own locking fail with `SQLITE_BUSY`. The lock is released when the
/// returned file is dropped or the process dies; the lock file stays behind, which is harmless.
///
/// File systems without `flock` support (some network mounts) open without a lock: refusing
/// to open would be worse than the unlikely double open.
pub(crate) fn lock_folder(root: &Path, lock_name: &str) -> Result<Option<File>, ProjectError> {
    let path = root.join(lock_name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .at(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Err(ProjectError::AlreadyOpen(root.to_owned())),
        Err(TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => Ok(None),
        Err(TryLockError::Error(source)) => Err(ProjectError::Io { path, source }),
    }
}

/// Joins a `/`-separated relative path (already validated) onto `base`.
pub(crate) fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_owned(), |p, c| p.join(c))
}
