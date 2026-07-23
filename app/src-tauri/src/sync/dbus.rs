//! Session D-Bus service consumed by the desktop widgets — and, as the owner
//! of a unique well-known name, the app's **single-instance guard**.
//!
//! Well-known name: `org.cirrust.client.Daemon` — deliberately NOT the bare app
//! identifier: Tauri's GTK layer registers `org.cirrust.client` itself as the
//! GApplication id, and sharing that name causes ownership fights.
//!
//! Object `/Sync`, interface `org.cirrust.client.Sync` with methods:
//!   - `Status() -> (state: s, activeFolder: s, folderCount: u, lastSync: s)`
//!   - `SyncNow()`
//!   - `Open()`
//!
//! The name is requested at startup with strict flags (no queueing, no
//! replacement). If it is already owned, another instance is running: we ask
//! it to raise its window via `Open()` and exit.

use super::{SyncState, SyncStatus};
use std::sync::OnceLock;
use tauri::AppHandle;
use tokio::sync::{mpsc, watch};
use zbus::interface;

const BUS_NAME: &str = "org.cirrust.client.Daemon";

/// The session-bus connection that owns [`BUS_NAME`]. Created on Tauri's
/// tokio runtime (which drives its I/O for the process lifetime) and kept
/// for as long as the app runs.
static BUS: OnceLock<zbus::Connection> = OnceLock::new();

pub enum BusAcquire {
    /// We own the name — we are the only instance.
    Primary,
    /// Another instance owns the name.
    AlreadyRunning,
    /// No usable session bus (continue without guard/widget service).
    NoBus,
}

/// Claim the app's well-known bus name with strict flags (no queueing, no
/// replacement). Must run on the app's long-lived async runtime.
pub async fn acquire_name() -> BusAcquire {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};

    let Ok(conn) = zbus::Connection::session().await else {
        return BusAcquire::NoBus;
    };
    let name = zbus::names::WellKnownName::try_from(BUS_NAME).expect("valid bus name");
    match conn
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
            let _ = BUS.set(conn);
            BusAcquire::Primary
        }
        Ok(_) | Err(zbus::Error::NameTaken) => BusAcquire::AlreadyRunning,
        Err(_) => BusAcquire::NoBus,
    }
}

/// Ask the running instance to show its window (used by a blocked second
/// launch before exiting).
pub async fn raise_running_instance() {
    if let Ok(conn) = zbus::Connection::session().await {
        let _ = conn
            .call_method(
                Some(BUS_NAME),
                "/Sync",
                Some("org.cirrust.client.Sync"),
                "Open",
                &(),
            )
            .await;
    }
}

pub struct SyncService {
    app: AppHandle,
    trigger: mpsc::UnboundedSender<()>,
    status: watch::Receiver<SyncStatus>,
}

#[interface(name = "org.cirrust.client.Sync")]
impl SyncService {
    /// Current sync status as a flat tuple (D-Bus has no rich enums).
    async fn status(&self) -> (String, String, u32, String) {
        let s = self.status.borrow().clone();
        (
            state_string(s.state).to_string(),
            s.active_folder.unwrap_or_default(),
            s.folder_count as u32,
            s.last_sync.unwrap_or_default(),
        )
    }

    /// Trigger an immediate sync of all enabled folders.
    async fn sync_now(&self) {
        let _ = self.trigger.send(());
    }

    /// Raise / show the main application window.
    async fn open(&self) {
        crate::show_main_window(&self.app);
    }
}

fn state_string(state: SyncState) -> &'static str {
    match state {
        SyncState::Idle => "idle",
        SyncState::Syncing => "syncing",
        SyncState::Paused => "paused",
        SyncState::Error => "error",
        SyncState::Offline => "offline",
    }
}

/// Serve the widget interface on the name-owning connection acquired at
/// startup (see [`acquire_name`]).
pub async fn serve(
    app: AppHandle,
    trigger: mpsc::UnboundedSender<()>,
    status: watch::Receiver<SyncStatus>,
) -> zbus::Result<()> {
    let Some(conn) = BUS.get() else {
        return Err(zbus::Error::Failure("no session bus".into()));
    };
    let service = SyncService { app, trigger, status };
    conn.object_server().at("/Sync", service).await?;
    Ok(())
}
