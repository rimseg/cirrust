//! On-disk JSON cache for CalDAV/CardDAV data, mirroring the sync engine's
//! journal convention (plain JSON under the app data dir). Caching lets the
//! Calendar/Contacts views open instantly and work offline; a background
//! refresh reconciles against the server using each collection's CTag.
//!
//! Layout: `<app_data_dir>/pim/<account_id>/<name>.json`.

use crate::error::{AppError, AppResult};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

fn dir(app: &AppHandle, account_id: &str) -> AppResult<PathBuf> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::msg(format!("cannot resolve data dir: {e}")))?;
    let d = base.join("pim").join(account_id);
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Load a cached value, or `None` if it hasn't been written yet.
pub fn load<T: DeserializeOwned>(
    app: &AppHandle,
    account_id: &str,
    name: &str,
) -> AppResult<Option<T>> {
    let path = dir(app, account_id)?.join(format!("{name}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Persist a value (atomic-ish: write to a temp file then rename).
pub fn save<T: Serialize>(
    app: &AppHandle,
    account_id: &str,
    name: &str,
    value: &T,
) -> AppResult<()> {
    let d = dir(app, account_id)?;
    let path = d.join(format!("{name}.json"));
    let tmp = d.join(format!(".{name}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(value)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Sanitize a collection id / url segment into a safe cache-file component.
pub fn safe_name(prefix: &str, id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{prefix}-{cleaned}")
}
