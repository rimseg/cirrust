//! Tauri commands backing the file browser: list, delete, download, upload,
//! create-folder and move/rename.

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::webdav::FileEntry;
use std::path::Path;
use tauri::State;

const MAX_TEXT_PREVIEW: usize = 2_000_000;

/// List the immediate children of a DAV directory. Use `"/"` for the root.
#[tauri::command]
pub async fn files_list(state: State<'_, AppState>, path: String) -> AppResult<Vec<FileEntry>> {
    let client = state.client().await?;
    client.list(&path).await
}

/// Recursively search for files/folders whose name contains `query`, under
/// `scope` (use `"/"` for the whole account). Capped server-side.
#[tauri::command]
pub async fn files_search(
    state: State<'_, AppState>,
    query: String,
    scope: String,
) -> AppResult<Vec<FileEntry>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let client = state.client().await?;
    client.search(q, &scope, 200).await
}

/// Delete a file or directory on the server.
#[tauri::command]
pub async fn files_delete(state: State<'_, AppState>, path: String) -> AppResult<()> {
    let client = state.client().await?;
    client.delete(&path).await
}

/// Download a remote file to an absolute local path.
#[tauri::command]
pub async fn files_download(
    state: State<'_, AppState>,
    path: String,
    local_path: String,
) -> AppResult<()> {
    let client = state.client().await?;
    let bytes = client.get_bytes(&path).await?;
    tokio::fs::write(&local_path, bytes).await?;
    Ok(())
}

/// Upload one or more local files into a remote directory (keeping their names).
#[tauri::command]
pub async fn files_upload(
    state: State<'_, AppState>,
    remote_dir: String,
    local_paths: Vec<String>,
) -> AppResult<()> {
    let client = state.client().await?;
    let base = remote_dir.trim_end_matches('/');
    for lp in local_paths {
        let name = Path::new(&lp)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let dest = format!("{base}/{name}");
        let bytes = tokio::fs::read(&lp).await?;
        client.put_bytes(&dest, bytes).await?;
    }
    Ok(())
}

/// Create a new folder (collection).
#[tauri::command]
pub async fn files_mkdir(state: State<'_, AppState>, path: String) -> AppResult<()> {
    let client = state.client().await?;
    client.mkcol(&path).await
}

/// Rename or move a file/folder.
#[tauri::command]
pub async fn files_move(state: State<'_, AppState>, from: String, to: String) -> AppResult<()> {
    let client = state.client().await?;
    client.move_to(&from, &to).await
}

/// Copy a file/folder to a new path.
#[tauri::command]
pub async fn files_copy(state: State<'_, AppState>, from: String, to: String) -> AppResult<()> {
    let client = state.client().await?;
    client.copy_to(&from, &to).await
}

/// Read a (small) text file for previewing. Rejects files that are too large.
#[tauri::command]
pub async fn files_read_text(state: State<'_, AppState>, path: String) -> AppResult<String> {
    let client = state.client().await?;
    let bytes = client.get_bytes(&path).await?;
    if bytes.len() > MAX_TEXT_PREVIEW {
        return Err(AppError::msg("file is too large to preview"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
