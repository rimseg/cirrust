//! Process-wide, thread-safe session state: one authenticated WebDAV client per
//! connected account, plus which account is currently being browsed.

use crate::config::Account;
use crate::error::{AppError, AppResult};
use crate::webdav::WebDavClient;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// One authenticated account + its ready-to-use WebDAV client.
pub struct Session {
    pub account: Account,
    pub client: WebDavClient,
}

#[derive(Default)]
struct Inner {
    /// account id → live session.
    sessions: HashMap<String, Session>,
    /// The account currently browsed in Files/Overview/Trash.
    active: Option<String>,
}

#[derive(Default)]
pub struct AppState {
    inner: RwLock<Inner>,
}

impl AppState {
    pub async fn add_session(&self, session: Session) {
        let mut g = self.inner.write().await;
        let id = session.account.id.clone();
        if g.active.is_none() {
            g.active = Some(id.clone());
        }
        g.sessions.insert(id, session);
    }

    pub async fn remove_session(&self, account_id: &str) {
        let mut g = self.inner.write().await;
        g.sessions.remove(account_id);
        if g.active.as_deref() == Some(account_id) {
            g.active = g.sessions.keys().next().cloned();
        }
    }

    #[allow(dead_code)] // reserved: "disconnect all accounts"
    pub async fn clear_all(&self) {
        let mut g = self.inner.write().await;
        g.sessions.clear();
        g.active = None;
    }

    pub async fn set_active(&self, account_id: &str) -> bool {
        let mut g = self.inner.write().await;
        if g.sessions.contains_key(account_id) {
            g.active = Some(account_id.to_string());
            true
        } else {
            false
        }
    }

    #[allow(dead_code)] // reserved for account-scoped stream URLs
    pub async fn active_id(&self) -> Option<String> {
        self.inner.read().await.active.clone()
    }

    /// The account currently being browsed, if any.
    pub async fn active_account(&self) -> Option<Account> {
        let g = self.inner.read().await;
        g.active.as_ref().and_then(|id| g.sessions.get(id)).map(|s| s.account.clone())
    }

    /// Every connected account.
    pub async fn accounts(&self) -> Vec<Account> {
        self.inner.read().await.sessions.values().map(|s| s.account.clone()).collect()
    }

    #[allow(dead_code)] // reserved for UI/session checks
    pub async fn is_authenticated(&self) -> bool {
        !self.inner.read().await.sessions.is_empty()
    }

    /// A cheap clone of the **active** account's WebDAV client (Files, previews,
    /// dashboard, trash, sharing all operate on the browsed account).
    pub async fn client(&self) -> AppResult<WebDavClient> {
        let g = self.inner.read().await;
        g.active
            .as_ref()
            .and_then(|id| g.sessions.get(id))
            .map(|s| s.client.clone())
            .ok_or(AppError::NotAuthenticated)
    }

    /// The WebDAV client for a specific account — used by the sync loop, which
    /// syncs each folder against the account it's bound to.
    pub async fn client_for(&self, account_id: &str) -> AppResult<WebDavClient> {
        self.inner
            .read()
            .await
            .sessions
            .get(account_id)
            .map(|s| s.client.clone())
            .ok_or(AppError::NotAuthenticated)
    }
}
