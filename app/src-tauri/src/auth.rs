//! Authentication for one or more Nextcloud/ownCloud accounts.
//!
//! Nextcloud accounts use **Login Flow v2** (browser approval). Any server —
//! Nextcloud *or* ownCloud — can also be added manually with a server URL,
//! username and an app password created in the server's security settings.
//! App passwords are persisted in the OS keyring (KWallet / Secret Service).

use crate::config::{Account, AppConfig, ServerKind};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Session};
use crate::webdav::WebDavClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

/// Sent as the User-Agent; the server shows this as the app-password device name.
pub const DEVICE_NAME: &str = "Cirrust";
const KEYRING_SERVICE: &str = "org.cirrust.client";

// ---------------------------------------------------------------------------
// Keyring helpers
// ---------------------------------------------------------------------------

// The keyring's Secret-Service backend talks blocking D-Bus (zbus), and with
// zbus's tokio feature enabled that spins up a runtime under the hood. Doing
// that ON a tokio worker panics ("Cannot start a runtime from within a
// runtime") and silently kills whichever background task made the call — so
// every keyring operation is pushed onto a dedicated blocking thread.

fn keyring_entry(account: &Account) -> AppResult<keyring::Entry> {
    let user = format!("{}@{}", account.username, account.server_url);
    Ok(keyring::Entry::new(KEYRING_SERVICE, &user)?)
}

async fn on_keyring_thread<T: Send + 'static>(
    account: &Account,
    op: impl FnOnce(keyring::Entry) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let account = account.clone();
    tokio::task::spawn_blocking(move || op(keyring_entry(&account)?))
        .await
        .map_err(|e| AppError::msg(format!("keyring task failed: {e}")))?
}

pub async fn store_password(account: &Account, password: &str) -> AppResult<()> {
    let password = password.to_string();
    on_keyring_thread(account, move |entry| Ok(entry.set_password(&password)?)).await
}

pub async fn load_password(account: &Account) -> AppResult<String> {
    on_keyring_thread(account, |entry| Ok(entry.get_password()?)).await
}

pub async fn delete_password(account: &Account) -> AppResult<()> {
    on_keyring_thread(account, |entry| match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    })
    .await
}

/// Restore all connected accounts at startup from config + keyring. Best-effort:
/// an account whose password is missing is simply skipped. Returns how many
/// sessions were restored.
pub async fn restore_sessions(app: &AppHandle, state: &AppState) -> AppResult<usize> {
    let cfg = AppConfig::load(app)?;
    // Persist the migrated shape (legacy single `account` → `accounts`, folders
    // bound to an account) so it's written once in the new format.
    let _ = cfg.save(app);
    let mut restored = 0;
    for account in &cfg.accounts {
        let Ok(password) = load_password(account).await else { continue };
        let Ok(client) = WebDavClient::new(account, password) else { continue };
        state.add_session(Session { account: account.clone(), client }).await;
        restored += 1;
    }
    if let Some(active) = cfg.active_account {
        state.set_active(&active).await;
    }
    Ok(restored)
}

/// Persist a freshly-connected account into config (idempotent) and make it the
/// active one if none is set yet.
fn persist_account(app: &AppHandle, account: &Account) -> AppResult<()> {
    let mut cfg = AppConfig::load(app)?;
    if !cfg.accounts.iter().any(|a| a.id == account.id) {
        cfg.accounts.push(account.clone());
    }
    if cfg.active_account.is_none() {
        cfg.active_account = Some(account.id.clone());
    }
    cfg.save(app)
}

fn normalize_server_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn http_client() -> AppResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(DEVICE_NAME)
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(30))
        .build()?)
}

// ---------------------------------------------------------------------------
// Login Flow v2 (Nextcloud)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LoginV2Response {
    poll: PollInfo,
    login: String,
}

#[derive(Debug, Deserialize)]
struct PollInfo {
    token: String,
    endpoint: String,
}

/// Handed to the UI so it can open the browser and start polling.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginFlowInit {
    pub login_url: String,
    pub poll_token: String,
    pub poll_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct PollSuccess {
    server: String,
    #[serde(rename = "loginName")]
    login_name: String,
    #[serde(rename = "appPassword")]
    app_password: String,
}

/// Begin Login Flow v2 against the given server.
#[tauri::command]
pub async fn auth_start_login(server_url: String) -> AppResult<LoginFlowInit> {
    let server = normalize_server_url(&server_url);
    let http = http_client()?;

    let resp = http.post(format!("{server}/index.php/login/v2")).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Server { status: status.as_u16(), body });
    }
    let parsed: LoginV2Response = resp.json().await?;
    Ok(LoginFlowInit {
        login_url: parsed.login,
        poll_token: parsed.poll.token,
        poll_endpoint: parsed.poll.endpoint,
    })
}

/// Poll once. Returns `Some(account)` once the user approves (persisting the
/// account + activating its session), or `None` while still pending.
#[tauri::command]
pub async fn auth_poll_login(
    app: AppHandle,
    state: State<'_, AppState>,
    poll_endpoint: String,
    poll_token: String,
) -> AppResult<Option<Account>> {
    let http = http_client()?;
    let resp = http
        .post(&poll_endpoint)
        .form(&[("token", poll_token.as_str())])
        .send()
        .await?;

    // The server returns 404 while the user has not finished logging in.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Server { status: status.as_u16(), body });
    }

    let success: PollSuccess = resp.json().await?;
    let account = Account::new(
        success.server.trim_end_matches('/').to_string(),
        success.login_name,
        ServerKind::Nextcloud,
    );

    store_password(&account, &success.app_password).await?;
    let client = WebDavClient::new(&account, success.app_password)?;
    state.add_session(Session { account: account.clone(), client }).await;
    persist_account(&app, &account)?;
    Ok(Some(account))
}

// ---------------------------------------------------------------------------
// Manual connect (Nextcloud or ownCloud, via app password)
// ---------------------------------------------------------------------------

/// Connect an account with an explicit server URL + username + app password.
/// Works for both Nextcloud and ownCloud. Credentials are validated with a
/// PROPFIND before they're saved.
#[tauri::command]
pub async fn auth_add_manual(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    username: String,
    password: String,
    kind: ServerKind,
) -> AppResult<Account> {
    let server = normalize_server_url(&server_url);
    let account = Account::new(server, username.trim().to_string(), kind);
    let client = WebDavClient::new(&account, password.clone())?;

    // Validate: a PROPFIND on the DAV root fails with 401 on bad credentials.
    client.list("/").await.map_err(|e| match e {
        AppError::Server { status: 401, .. } => {
            AppError::msg("Wrong username or app password")
        }
        other => other,
    })?;

    store_password(&account, &password).await?;
    state.add_session(Session { account: account.clone(), client }).await;
    persist_account(&app, &account)?;
    Ok(account)
}

// ---------------------------------------------------------------------------
// Account management
// ---------------------------------------------------------------------------

/// All connected accounts.
#[tauri::command]
pub async fn auth_list_accounts(state: State<'_, AppState>) -> AppResult<Vec<Account>> {
    Ok(state.accounts().await)
}

/// The account currently being browsed.
#[tauri::command]
pub async fn auth_active_account(state: State<'_, AppState>) -> AppResult<Option<Account>> {
    Ok(state.active_account().await)
}

/// Switch which account Files/Overview/Trash browse.
#[tauri::command]
pub async fn auth_set_active_account(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<()> {
    if state.set_active(&account_id).await {
        let mut cfg = AppConfig::load(&app)?;
        cfg.active_account = Some(account_id);
        cfg.save(&app)?;
    }
    Ok(())
}

/// Disconnect one account: drop its session, wipe its keyring entry, remove it
/// (and its synced folders) from config.
#[tauri::command]
pub async fn auth_remove_account(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app)?;
    if let Some(account) = cfg.account_by_id(&account_id).cloned() {
        let _ = delete_password(&account).await;
    }
    state.remove_session(&account_id).await;

    cfg.accounts.retain(|a| a.id != account_id);
    cfg.sync_folders.retain(|f| f.account_id != account_id);
    if cfg.active_account.as_deref() == Some(account_id.as_str()) {
        cfg.active_account = cfg.accounts.first().map(|a| a.id.clone());
    }
    cfg.save(&app)?;
    Ok(())
}
