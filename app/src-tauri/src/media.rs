//! Media resolution for previews + the audio player.
//!
//! A sync client usually already has the user's media on disk, so playback
//! should read the **real local file** through Tauri's asset protocol (native
//! range + seek support) instead of buffering a whole file through `stream://`.
//! These commands map a DAV path to its synced local copy, and — when a file
//! isn't synced — download it into the app cache so it can still play from a
//! real file rather than an unreliable custom-scheme stream.

use crate::config::AppConfig;
use crate::error::AppResult;
use crate::state::AppState;
use sha1::Digest;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

/// Map a DAV path (relative to the account's WebDAV root, e.g. `/Music/a.mp3`)
/// to an existing local file inside a synced folder. Returns the absolute local
/// path only when the file is actually present on disk.
///
/// Sync folders for the **active** account are preferred; others are a fallback
/// (paths rarely collide across accounts, and any existing file is still a
/// faithful copy of the requested content).
#[tauri::command]
pub async fn media_local_path(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> AppResult<Option<String>> {
    let active = state.active_id().await;
    Ok(resolve_local(&app, active.as_deref(), &path))
}

/// Local path for a DAV `path` if it exists on disk — a synced **file OR
/// directory** (including a synced folder's root). Used by "Open in file
/// manager", which reveals the item in the desktop file manager.
#[tauri::command]
pub async fn media_reveal_path(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> AppResult<Option<String>> {
    let active = state.active_id().await;
    Ok(resolve_local_any(&app, active.as_deref(), &path))
}

/// Like [`resolve_local`] but matches directories too (and the folder root), so
/// a synced folder can be opened, not just a file.
fn resolve_local_any(app: &AppHandle, active: Option<&str>, dav_path: &str) -> Option<String> {
    let cfg = AppConfig::load(app).ok()?;
    let mut folders: Vec<_> = cfg.sync_folders.iter().filter(|f| f.enabled).collect();
    folders.sort_by_key(|f| active != Some(f.account_id.as_str()));
    for f in folders {
        let Some(rel) = dav_under(&f.remote_path, dav_path) else {
            continue;
        };
        let local = if rel.is_empty() {
            PathBuf::from(&f.local_path)
        } else {
            PathBuf::from(&f.local_path).join(&rel)
        };
        if local.exists() {
            return Some(local.to_string_lossy().into_owned());
        }
    }
    None
}

/// Return a local file for `path`, downloading it into the app cache when it is
/// not synced locally. The returned path is safe to hand to `convertFileSrc`.
///
/// `etag` (when known) is folded into the cache filename so a changed remote
/// file re-caches instead of replaying stale bytes.
#[tauri::command]
pub async fn media_cache(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    etag: Option<String>,
) -> AppResult<String> {
    // Synced copy already on disk → use it directly, no download.
    let active = state.active_id().await;
    if let Some(local) = resolve_local(&app, active.as_deref(), &path) {
        return Ok(local);
    }

    let mut dir = app.path().app_cache_dir().map_err(|e| {
        crate::error::AppError::msg(format!("cannot resolve cache dir: {e}"))
    })?;
    dir.push("media");
    tokio::fs::create_dir_all(&dir).await?;

    let base = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    let key = short_hash(&format!("{path}\u{0}{}", etag.as_deref().unwrap_or("")));
    let dest = dir.join(format!("{key}-{base}"));

    // Already cached from a previous play → reuse.
    if tokio::fs::metadata(&dest).await.map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(dest.to_string_lossy().into_owned());
    }

    // Download into a temp sibling, then atomically rename — a crashed download
    // never leaves a truncated file masquerading as a complete cache entry.
    let tmp = dir.join(format!("{key}-{base}.part"));
    let client = state.client().await?;
    client.download_to_file(&path, &tmp, |_, _| {}).await?;
    tokio::fs::rename(&tmp, &dest).await?;
    Ok(dest.to_string_lossy().into_owned())
}

/// Return an `http://127.0.0.1` URL that plays `path` through the loopback media
/// server (see `mediahttp.rs`). The file must exist on disk first: a synced copy
/// is used directly, otherwise it's downloaded into the media cache. This is the
/// source `<video>`/`<audio>` elements use — a real HTTP origin is the only one
/// WebKitGTK will seek (custom-scheme and Blob URLs both fail).
#[tauri::command]
pub async fn media_http_url(
    app: AppHandle,
    state: State<'_, AppState>,
    server: State<'_, crate::mediahttp::MediaServer>,
    path: String,
    etag: Option<String>,
) -> AppResult<String> {
    let local = match resolve_local(&app, state.active_id().await.as_deref(), &path) {
        Some(p) => p,
        // Not synced → reuse the cache-download path (temp file + atomic rename).
        None => media_cache(app.clone(), state, path, etag).await?,
    };
    Ok(server.url_for(&local))
}

/// Return the raw bytes of a media file for playback via a Blob URL. Reads the
/// synced local copy when present, else downloads from the server. Returned as
/// a binary IPC `Response` (an `ArrayBuffer` on the JS side) so the bytes aren't
/// serialized through JSON.
///
/// This exists because WebKitGTK's `<audio>`/`<video>` loader over a custom URI
/// scheme doesn't reliably issue follow-up Range requests, leaving playback
/// stuck (MediaError code 4). A Blob URL decodes straight from memory with no
/// streaming pipeline, so it just works.
#[tauri::command]
pub async fn media_bytes(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> AppResult<tauri::ipc::Response> {
    let active = state.active_id().await;
    let data = if let Some(local) = resolve_local(&app, active.as_deref(), &path) {
        tokio::fs::read(&local).await?
    } else {
        state.client().await?.get_bytes(&path).await?
    };
    Ok(tauri::ipc::Response::new(data))
}

/// Load the config and resolve `dav_path` against the enabled sync folders,
/// preferring the active account's folders. Returns an existing local file.
fn resolve_local(app: &AppHandle, active: Option<&str>, dav_path: &str) -> Option<String> {
    let cfg = AppConfig::load(app).ok()?;
    let mut folders: Vec<_> = cfg.sync_folders.iter().filter(|f| f.enabled).collect();
    // Active-account folders first so an ambiguous path resolves to the folder
    // the user is actually browsing.
    folders.sort_by_key(|f| active != Some(f.account_id.as_str()));

    for f in folders {
        let Some(rel) = dav_under(&f.remote_path, dav_path) else {
            continue;
        };
        if rel.is_empty() {
            continue; // the folder root itself, not a file
        }
        let local = PathBuf::from(&f.local_path).join(&rel);
        if local.is_file() {
            return Some(local.to_string_lossy().into_owned());
        }
    }
    None
}

/// If `dav` lies under `remote_base`, return its path relative to the base
/// (mirrors `sync/engine.rs`'s join/relativise convention). `None` otherwise.
fn dav_under(remote_base: &str, dav: &str) -> Option<String> {
    let b = remote_base.trim_end_matches('/');
    let p = dav.trim_end_matches('/');
    if b.is_empty() {
        return Some(p.trim_start_matches('/').to_string());
    }
    if p == b {
        return Some(String::new());
    }
    p.strip_prefix(&format!("{b}/")).map(str::to_string)
}

/// First 16 hex chars of the SHA-1 of `s` — a compact, stable cache key.
fn short_hash(s: &str) -> String {
    let mut h = sha1::Sha1::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::dav_under;

    #[test]
    fn maps_paths_under_a_folder() {
        assert_eq!(dav_under("/Music", "/Music/a/b.mp3"), Some("a/b.mp3".into()));
        assert_eq!(dav_under("/Music/", "/Music/song.mp3"), Some("song.mp3".into()));
        assert_eq!(dav_under("/", "/song.mp3"), Some("song.mp3".into()));
        assert_eq!(dav_under("/Music", "/Music"), Some(String::new()));
        // Not under the folder, and prefix-only matches must not leak through.
        assert_eq!(dav_under("/Music", "/Photos/x.jpg"), None);
        assert_eq!(dav_under("/Music", "/MusicVideos/x.mp4"), None);
    }
}
