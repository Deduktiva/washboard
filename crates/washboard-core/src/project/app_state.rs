//! App-level state: which projects were open, for restoring them on launch (PLAN §3).
//!
//! Lives in `<dir>/state.json`, where the app passes `~/Library/Application Support/Washboard`.
//! JSON, not SQLite: it is tiny, written on quit, and worth being readable when debugging.
//! Unknown fields are ignored so an older build can read a newer file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::fsutil::atomic_write;
use crate::model::RequestId;

pub const STATE_FILE: &str = "state.json";
const FORMAT_VERSION: u32 = 1;
/// How many entries [`AppState::note_recent`] keeps.
pub const MAX_RECENT: usize = 10;

#[derive(Debug, Error)]
pub enum AppStateError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    /// The file exists but is not valid; the app should start with an empty state.
    #[error("{} is not valid: {source}", path.display())]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Projects open at quit, in window order.
    pub open_projects: Vec<OpenProject>,
    /// For the welcome window and File ▸ Open Recent, most recent first.
    pub recent_projects: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProject {
    pub path: PathBuf,
    /// Security-scoped bookmark data. Stored from day one so enabling the App Sandbox later
    /// needs no migration; unused (may be empty) until then.
    #[serde(default)]
    pub bookmark: Option<Vec<u8>>,
    #[serde(default)]
    pub window_autosave_name: Option<String>,
    #[serde(default, with = "opt_request_id")]
    pub last_selected_request: Option<RequestId>,
}

impl OpenProject {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            bookmark: None,
            window_autosave_name: None,
            last_selected_request: None,
        }
    }
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    #[serde(flatten)]
    state: &'a AppState,
}

#[derive(Deserialize)]
struct FileIn {
    #[serde(flatten)]
    state: AppState,
}

impl AppState {
    /// Reads `<dir>/state.json`. A missing file is an empty state.
    pub fn load(dir: &Path) -> Result<AppState, AppStateError> {
        let path = dir.join(STATE_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AppState::default()),
            Err(source) => return Err(AppStateError::Io { path, source }),
        };
        serde_json::from_slice::<FileIn>(&bytes)
            .map(|f| f.state)
            .map_err(|source| AppStateError::Invalid { path, source })
    }

    /// Writes `<dir>/state.json` atomically, creating `dir` if needed.
    pub fn save(&self, dir: &Path) -> Result<(), AppStateError> {
        fs::create_dir_all(dir).map_err(|source| AppStateError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let path = dir.join(STATE_FILE);
        let json = serde_json::to_vec_pretty(&FileOut {
            version: FORMAT_VERSION,
            state: self,
        })
        .map_err(|source| AppStateError::Invalid {
            path: path.clone(),
            source,
        })?;
        atomic_write(&path, &json).map_err(|e| match e {
            super::ProjectError::Io { path, source } => AppStateError::Io { path, source },
            other => AppStateError::Io {
                path: path.clone(),
                source: io::Error::other(other.to_string()),
            },
        })
    }

    /// Moves `path` to the front of the recent list, keeping at most [`MAX_RECENT`].
    pub fn note_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        self.recent_projects.insert(0, path.to_owned());
        self.recent_projects.truncate(MAX_RECENT);
    }
}

/// `RequestId` has no serde support (the model stays serde-free); store the UUID string.
mod opt_request_id {
    use super::{RequestId, Uuid};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<RequestId>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(id) => s.serialize_some(&id.to_string()),
            None => s.serialize_none(),
        }
    }

    /// An unparseable id reads as "nothing selected" rather than failing the whole file.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<RequestId>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(s.and_then(|s| Uuid::parse_str(&s).ok()).map(RequestId))
    }
}
