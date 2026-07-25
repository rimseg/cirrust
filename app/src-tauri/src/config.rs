//! Persistent, non-secret application configuration: the connected accounts and
//! the synced folders. App passwords are **never** stored here — they live in
//! the OS keyring (see [`crate::auth`]).

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// The server platform an account connects to. Nextcloud and ownCloud share the
/// same WebDAV/OCS surface (the `oc:` DAV namespace is ownCloud's), so the sync
/// engine treats them identically; this only affects the login flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    #[default]
    Nextcloud,
    Owncloud,
}

/// A connected account (identity only, no secret).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Stable identifier, derived from `username@server_url`.
    #[serde(default)]
    pub id: String,
    /// Base server URL, e.g. `https://cloud.example.com` (no trailing slash).
    pub server_url: String,
    /// The login name (Login Flow v2 or manually entered).
    pub username: String,
    #[serde(default)]
    pub kind: ServerKind,
}

impl Account {
    pub fn new(server_url: String, username: String, kind: ServerKind) -> Self {
        let mut a = Account { id: String::new(), server_url, username, kind };
        a.ensure_id();
        a
    }

    /// Compute the stable id if it's missing (migration / older configs).
    pub fn ensure_id(&mut self) {
        if self.id.is_empty() {
            self.id = account_id(&self.server_url, &self.username);
        }
    }
}

/// Deterministic, stable id for an account so re-adding the same server+user
/// doesn't duplicate it and migrations are reproducible.
pub fn account_id(server_url: &str, username: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(username.as_bytes());
    h.update(b"@");
    h.update(server_url.trim_end_matches('/').as_bytes());
    hex::encode(&h.finalize()[..8])
}

/// One folder pair kept in sync between the local disk and a server.
///
/// Sync is always **two-way** — the reconciliation matrix is hard enough to
/// get right for one mode, and partial modes multiplied the states in which a
/// mistake destroys data. (A short-lived `direction` field existed once;
/// configs that still carry it are read fine — serde ignores unknown fields.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFolder {
    pub id: String,
    /// Which [`Account`] this folder syncs against.
    #[serde(default)]
    pub account_id: String,
    /// Absolute local directory path.
    pub local_path: String,
    /// Remote path relative to the user's WebDAV root, e.g. `/Music`.
    pub remote_path: String,
    /// When false the folder is registered but not actively synced.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// All connected accounts.
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// The account currently browsed in Files/Overview/Trash (by id).
    #[serde(default)]
    pub active_account: Option<String>,
    /// Legacy single-account field; migrated into `accounts` on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(default)]
    pub sync_folders: Vec<SyncFolder>,
    /// Global pause — when true the engine skips all runs.
    #[serde(default)]
    pub paused: bool,
    /// Glob-ish patterns; matching files/dirs are never synced.
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
}

impl AppConfig {
    fn path(app: &AppHandle) -> AppResult<PathBuf> {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|e| AppError::msg(format!("cannot resolve config dir: {e}")))?;
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("config.json"))
    }

    pub fn load(app: &AppHandle) -> AppResult<Self> {
        let path = Self::path(app)?;
        let mut cfg: Self = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.migrate();
        Ok(cfg)
    }

    /// Fold a legacy single `account` into the `accounts` list, backfill account
    /// ids, bind orphan folders to a default account, and pick an active one.
    fn migrate(&mut self) {
        for a in &mut self.accounts {
            a.ensure_id();
        }
        if let Some(mut old) = self.account.take() {
            old.ensure_id();
            if !self.accounts.iter().any(|a| a.id == old.id) {
                self.accounts.push(old);
            }
        }
        if self.active_account.as_deref().map_or(true, |id| {
            !self.accounts.iter().any(|a| a.id == id)
        }) {
            self.active_account = self.accounts.first().map(|a| a.id.clone());
        }
        // Bind folders that predate account tagging to the active account.
        if let Some(default_id) = self.active_account.clone() {
            for f in &mut self.sync_folders {
                if f.account_id.is_empty() {
                    f.account_id = default_id.clone();
                }
            }
        }
    }

    pub fn account_by_id(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn save(&self, app: &AppHandle) -> AppResult<()> {
        let path = Self::path(app)?;
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}
