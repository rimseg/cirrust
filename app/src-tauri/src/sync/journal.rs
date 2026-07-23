//! Per-folder sync journal: the last-synced state of every path, used as the
//! three-way merge base so we can distinguish *created* / *modified* / *deleted*
//! on each side. Persisted as JSON under a caller-provided journals directory.

use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The recorded state of one path at the time it was last synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub is_dir: bool,
    /// Remote ETag last seen (files only).
    pub etag: Option<String>,
    /// Local size last seen.
    pub size: u64,
    /// Local mtime (unix seconds) last seen.
    pub local_mtime: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Journal {
    /// Keyed by path relative to the folder root, no leading/trailing slash
    /// (e.g. `sub/song.mp3`).
    pub entries: HashMap<String, JournalEntry>,
}

impl Journal {
    fn file(dir: &Path, folder_id: &str) -> PathBuf {
        dir.join(format!("{folder_id}.json"))
    }

    pub fn load(dir: &Path, folder_id: &str) -> AppResult<Self> {
        match std::fs::read(Self::file(dir, folder_id)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, dir: &Path, folder_id: &str) -> AppResult<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(Self::file(dir, folder_id), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn delete(dir: &Path, folder_id: &str) {
        let _ = std::fs::remove_file(Self::file(dir, folder_id));
    }
}
