//! Account overview: identity, storage quota, server info, and the server-side
//! activity feed — all via Nextcloud's OCS API + `/status.php`.

use crate::error::AppResult;
use crate::state::AppState;
use serde::Serialize;
use serde_json::Value;
use tauri::State;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub display_name: String,
    pub email: Option<String>,
    pub server_url: String,
    pub server_version: Option<String>,
    pub product_name: Option<String>,
    /// Bytes used. Quota total is `-1` when unlimited/unknown.
    pub quota_used: i64,
    pub quota_total: i64,
    pub quota_free: i64,
    /// Percentage used (0–100).
    pub quota_relative: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    pub subject: String,
    pub message: Option<String>,
    pub time: String,
    pub activity_type: String,
    pub object_name: Option<String>,
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

#[tauri::command]
pub async fn account_info(state: State<'_, AppState>) -> AppResult<AccountInfo> {
    let client = state.client().await?;

    let status = client.plain_json("/status.php").await.ok();
    let user = client.ocs_json("/ocs/v2.php/cloud/user?format=json").await?;
    let data = &user["ocs"]["data"];
    let quota = &data["quota"];

    Ok(AccountInfo {
        display_name: str_field(data, &["display-name", "displayname"]).unwrap_or_default(),
        email: str_field(data, &["email"]),
        server_url: client.server_base().to_string(),
        server_version: status.as_ref().and_then(|s| str_field(s, &["versionstring", "version"])),
        product_name: status.as_ref().and_then(|s| str_field(s, &["productname"])),
        quota_used: quota["used"].as_i64().unwrap_or(0),
        quota_total: quota["total"].as_i64().unwrap_or(-1),
        quota_free: quota["free"].as_i64().unwrap_or(0),
        quota_relative: quota["relative"].as_f64().unwrap_or(0.0),
    })
}

/// Recent server-side activity. Empty if the Activity app is unavailable.
#[tauri::command]
pub async fn account_activity(state: State<'_, AppState>) -> AppResult<Vec<ActivityItem>> {
    let client = state.client().await?;
    let v = match client
        .ocs_json("/ocs/v2.php/apps/activity/api/v2/activity?format=json&limit=30")
        .await
    {
        Ok(v) => v,
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    if let Some(arr) = v["ocs"]["data"].as_array() {
        for a in arr {
            out.push(ActivityItem {
                subject: a["subject"].as_str().unwrap_or_default().to_string(),
                message: str_field(a, &["message"]),
                time: a["datetime"].as_str().unwrap_or_default().to_string(),
                activity_type: a["type"].as_str().unwrap_or_default().to_string(),
                object_name: str_field(a, &["object_name"]),
            });
        }
    }
    Ok(out)
}
