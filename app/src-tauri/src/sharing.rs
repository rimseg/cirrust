//! Public link sharing via Nextcloud's OCS Sharing API
//! (`/ocs/v2.php/apps/files_sharing/api/v1/shares`).

use crate::error::AppResult;
use crate::state::AppState;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use tauri::State;

const SHARES: &str = "/ocs/v2.php/apps/files_sharing/api/v1/shares";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    pub id: String,
    /// 3 = public link, 0 = user, 1 = group, …
    pub share_type: i64,
    pub url: Option<String>,
    pub token: Option<String>,
    pub path: String,
    pub permissions: i64,
    pub expiration: Option<String>,
    pub label: Option<String>,
    /// Recipient (user/group shares); `null` for public links.
    pub share_with: Option<String>,
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v[*k].as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn parse_share(v: &Value) -> Share {
    let id = v["id"]
        .as_str()
        .map(String::from)
        .or_else(|| v["id"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    Share {
        id,
        share_type: v["share_type"].as_i64().unwrap_or(-1),
        url: str_field(v, &["url"]),
        token: str_field(v, &["token"]),
        path: v["path"].as_str().unwrap_or_default().to_string(),
        permissions: v["permissions"].as_i64().unwrap_or(0),
        expiration: str_field(v, &["expiration"]),
        label: str_field(v, &["label"]),
        share_with: str_field(v, &["share_with_displayname", "share_with"]),
    }
}

/// List shares — all, or just those on `path`.
#[tauri::command]
pub async fn shares_list(
    state: State<'_, AppState>,
    path: Option<String>,
) -> AppResult<Vec<Share>> {
    let client = state.client().await?;
    let url = match &path {
        Some(p) => format!(
            "{SHARES}?format=json&reshares=false&path={}",
            utf8_percent_encode(p, NON_ALPHANUMERIC)
        ),
        None => format!("{SHARES}?format=json"),
    };
    let v = client.ocs_json(&url).await?;
    Ok(v["ocs"]["data"]
        .as_array()
        .map(|arr| arr.iter().map(parse_share).collect())
        .unwrap_or_default())
}

/// Create a public link for `path`, with optional password and expiry (YYYY-MM-DD).
#[tauri::command]
pub async fn share_create(
    state: State<'_, AppState>,
    path: String,
    password: Option<String>,
    expire_date: Option<String>,
) -> AppResult<Share> {
    let client = state.client().await?;
    let mut form: Vec<(&str, &str)> = vec![
        ("shareType", "3"),
        ("path", path.as_str()),
        ("permissions", "1"),
    ];
    if let Some(pw) = password.as_deref().filter(|p| !p.is_empty()) {
        form.push(("password", pw));
    }
    if let Some(exp) = expire_date.as_deref().filter(|e| !e.is_empty()) {
        form.push(("expireDate", exp));
    }
    let v = client
        .ocs_send(Method::POST, &format!("{SHARES}?format=json"), &form)
        .await?;
    Ok(parse_share(&v["ocs"]["data"]))
}

/// Revoke a share by id.
#[tauri::command]
pub async fn share_delete(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let client = state.client().await?;
    client
        .ocs_send(Method::DELETE, &format!("{SHARES}/{id}?format=json"), &[])
        .await?;
    Ok(())
}
