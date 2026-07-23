//! Nextcloud trash bin via the `trashbin` DAV endpoint: list deleted files,
//! restore them, delete permanently, or empty the bin.

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use reqwest::Method;
use serde::Serialize;
use tauri::State;

const NC_NS: &str = "http://nextcloud.org/ns";
const DAV_NS: &str = "DAV:";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    /// Id path inside the trashbin (e.g. `file.txt.d1720000000`).
    pub trash_id: String,
    pub name: String,
    /// Original location relative to the files root.
    pub original_location: String,
    /// Unix seconds of deletion.
    pub deleted_at: i64,
    pub size: u64,
    pub is_dir: bool,
}

pub async fn list_trash(client: &crate::webdav::WebDavClient) -> AppResult<Vec<TrashEntry>> {
    const BODY: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns">
  <d:prop>
    <d:resourcetype/>
    <d:getcontentlength/>
    <nc:trashbin-filename/>
    <nc:trashbin-original-location/>
    <nc:trashbin-deletion-time/>
  </d:prop>
</d:propfind>"#;

    let base = format!("trashbin/{}/trash", client.user());
    let resp = client
        .dav_request(Method::from_bytes(b"PROPFIND").unwrap(), &base)
        .header("Depth", "1")
        .header("Content-Type", "application/xml")
        .body(BODY)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::Server {
            status: status.as_u16(),
            body: text.chars().take(300).collect(),
        });
    }

    let doc = roxmltree::Document::parse(&text)
        .map_err(|e| AppError::msg(format!("bad trashbin xml: {e}")))?;

    let mut out = Vec::new();
    for response in doc.descendants().filter(|n| n.has_tag_name((DAV_NS, "response"))) {
        let href = response
            .children()
            .find(|n| n.has_tag_name((DAV_NS, "href")))
            .and_then(|n| n.text())
            .unwrap_or_default();
        let decoded = percent_encoding::percent_decode_str(href).decode_utf8_lossy();
        // Skip the trash root itself.
        let Some(tail) = decoded.split("/trash/").nth(1).filter(|t| !t.is_empty()) else {
            continue;
        };
        let trash_id = tail.trim_end_matches('/').to_string();

        let prop = response.descendants().find(|n| n.has_tag_name((DAV_NS, "prop")));
        let text_of = |ns: &str, tag: &str| -> Option<String> {
            prop.and_then(|p| p.descendants().find(|n| n.has_tag_name((ns, tag))))
                .and_then(|n| n.text())
                .map(String::from)
        };
        let is_dir = prop
            .and_then(|p| p.descendants().find(|n| n.has_tag_name((DAV_NS, "resourcetype"))))
            .map(|rt| rt.children().any(|n| n.has_tag_name((DAV_NS, "collection"))))
            .unwrap_or(false);

        out.push(TrashEntry {
            name: text_of(NC_NS, "trashbin-filename").unwrap_or_else(|| trash_id.clone()),
            original_location: text_of(NC_NS, "trashbin-original-location").unwrap_or_default(),
            deleted_at: text_of(NC_NS, "trashbin-deletion-time")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            size: text_of(DAV_NS, "getcontentlength")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            is_dir,
            trash_id,
        });
    }
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(out)
}

/// Restore a trashed file to its original location.
pub async fn restore(client: &crate::webdav::WebDavClient, trash_id: &str) -> AppResult<()> {
    let user = client.user().to_string();
    let dest = format!(
        "{}/remote.php/dav/trashbin/{}/restore/{}",
        client.server_base(),
        user,
        crate::webdav::encode_path(&trash_id)
    );
    let resp = client
        .dav_request(
            Method::from_bytes(b"MOVE").unwrap(),
            &format!("trashbin/{user}/trash/{trash_id}"),
        )
        .header("Destination", dest)
        .send()
        .await?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(AppError::Server { status: status.as_u16(), body: body.chars().take(300).collect() })
    }
}

/// Permanently delete one trashed file.
#[tauri::command]
pub async fn trash_delete(state: State<'_, AppState>, trash_id: String) -> AppResult<()> {
    let client = state.client().await?;
    let user = client.user().to_string();
    let resp = client
        .dav_request(Method::DELETE, &format!("trashbin/{user}/trash/{trash_id}"))
        .send()
        .await?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(AppError::Server { status: status.as_u16(), body: String::new() })
    }
}

/// Empty the whole trash bin.
#[tauri::command]
pub async fn trash_empty(state: State<'_, AppState>) -> AppResult<()> {
    let client = state.client().await?;
    let user = client.user().to_string();
    let resp = client
        .dav_request(Method::DELETE, &format!("trashbin/{user}/trash"))
        .send()
        .await?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(AppError::Server { status: status.as_u16(), body: String::new() })
    }
}

// Command wrappers around the testable core functions.

#[tauri::command]
pub async fn trash_list(state: State<'_, AppState>) -> AppResult<Vec<TrashEntry>> {
    let client = state.client().await?;
    list_trash(&client).await
}

#[tauri::command]
pub async fn trash_restore(state: State<'_, AppState>, trash_id: String) -> AppResult<()> {
    let client = state.client().await?;
    restore(&client, &trash_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Account;
    use crate::webdav::WebDavClient;

    /// Live: delete a file → find it in the trash → restore → verify it's back.
    #[tokio::test]
    #[ignore]
    async fn live_trash_roundtrip() {
        let account = Account::new(std::env::var("NC_URL").unwrap(), std::env::var("NC_USER").unwrap(), crate::config::ServerKind::Nextcloud);
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        let dir = "/nc_kde_trashtest";
        let file = "/nc_kde_trashtest/t.txt";
        let _ = client.delete(dir).await;
        client.mkcol(dir).await.unwrap();
        client.put_bytes(file, b"trash me".to_vec()).await.unwrap();
        client.delete(file).await.unwrap();

        let entries = list_trash(&client).await.unwrap();
        println!("trash has {} entries", entries.len());
        let mine = entries
            .iter()
            .find(|e| e.name == "t.txt" && e.original_location.contains("nc_kde_trashtest"))
            .expect("deleted file should be in trash");
        println!("found: {} (deleted at {})", mine.trash_id, mine.deleted_at);

        restore(&client, &mine.trash_id).await.unwrap();
        let back = client.stat(file).await.unwrap();
        assert!(back.is_some(), "file restored to original location");

        client.delete(dir).await.unwrap();
        println!("LIVE TRASH OK");
    }
}
