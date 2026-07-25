//! Folder-sync management: the background engine, its status surface, and the
//! Tauri commands used by the UI (and, later, the Plasma widget over D-Bus).

pub mod dbus;
mod engine;
mod journal;
mod progress;

use crate::config::{AppConfig, SyncFolder};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use engine::Cancel;
use journal::Journal;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::WalkDir;
use progress::{Activity, ActivityLog, Progress, Reporter};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};

/// Coarse sync state, mirrored to the widget.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncState {
    Idle,
    Syncing,
    Paused,
    Error,
    Offline,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub state: SyncState,
    pub active_folder: Option<String>,
    pub message: Option<String>,
    pub last_sync: Option<String>,
    pub folder_count: usize,
    pub paused: bool,
}

impl SyncStatus {
    fn new(count: usize) -> Self {
        SyncStatus {
            state: SyncState::Idle,
            active_folder: None,
            message: None,
            last_sync: None,
            folder_count: count,
            paused: false,
        }
    }
}

/// Ids of folders currently disabled ("paused") — shared live with the run
/// loop so disabling a folder cancels its in-flight sync, not just future ones.
type DisabledFolders = Arc<RwLock<HashSet<String>>>;

/// Owns the background sync task's control channels + live surfaces.
pub struct SyncManager {
    app: AppHandle,
    status_rx: watch::Receiver<SyncStatus>,
    status_tx: watch::Sender<SyncStatus>,
    progress_rx: watch::Receiver<Progress>,
    activity: ActivityLog,
    trigger: mpsc::UnboundedSender<()>,
    paused: Arc<AtomicBool>,
    disabled: DisabledFolders,
}

impl SyncManager {
    /// Spawn the background sync loop and return its handle.
    pub fn start(app: AppHandle) -> Self {
        let cfg0 = AppConfig::load(&app).unwrap_or_default();
        let count = cfg0.sync_folders.len();
        let paused = Arc::new(AtomicBool::new(cfg0.paused));
        let disabled: DisabledFolders = Arc::new(RwLock::new(
            cfg0.sync_folders.iter().filter(|f| !f.enabled).map(|f| f.id.clone()).collect(),
        ));
        let mut initial = SyncStatus::new(count);
        initial.paused = cfg0.paused;
        // Nothing is known about the server yet — no account has even been
        // restored at this point. Any state but Offline is a guess, and the
        // guess that misleads is "synced": a green tray on a fresh install
        // claims the data is safe when nothing has ever been contacted.
        initial.state = if cfg0.paused { SyncState::Paused } else { SyncState::Offline };
        let (status_tx, status_rx) = watch::channel(initial);
        let (progress_tx, progress_rx) = watch::channel(Progress::default());
        let (trigger, trigger_rx) = mpsc::unbounded_channel();
        let activity: ActivityLog = Arc::new(Mutex::new(VecDeque::new()));

        // Progress/activity aggregator.
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let reporter = Reporter::new(event_tx);
        tauri::async_runtime::spawn(progress::consume(
            app.clone(),
            event_rx,
            progress_tx,
            activity.clone(),
        ));

        // Serve the session D-Bus interface for the desktop widgets on the
        // name-owning connection acquired by the single-instance guard.
        {
            let app = app.clone();
            let trigger = trigger.clone();
            let status = status_rx.clone();
            tauri::async_runtime::spawn(async move {
                match dbus::serve(app, trigger, status).await {
                    Ok(()) => log::info!("d-bus service registered as org.cirrust.client.Daemon"),
                    Err(e) => log::warn!("d-bus service failed: {e}"),
                }
            });
        }

        // Keep the tray icon badge + tooltip in step with the sync state.
        crate::tray_badge::spawn_status_badge(app.clone(), status_rx.clone());

        // Connectivity watchdog — notices a dropped link between sync runs.
        let online = Arc::new(AtomicBool::new(true));
        tauri::async_runtime::spawn(health_loop(
            app.clone(),
            status_tx.clone(),
            trigger.clone(),
            paused.clone(),
            online.clone(),
        ));

        tauri::async_runtime::spawn(run_loop(
            app.clone(),
            trigger_rx,
            status_tx.clone(),
            reporter,
            paused.clone(),
            disabled.clone(),
            online,
        ));

        SyncManager { app, status_rx, status_tx, progress_rx, activity, trigger, paused, disabled }
    }

    pub fn status(&self) -> SyncStatus {
        self.status_rx.borrow().clone()
    }

    pub fn progress(&self) -> Progress {
        self.progress_rx.borrow().clone()
    }

    pub fn activity(&self) -> Vec<Activity> {
        self.activity
            .lock()
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    pub fn kick(&self) {
        let _ = self.trigger.send(());
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        // Publish the new state synchronously. A running sync only *notices*
        // the flag at its next cancellation point, and the old behaviour —
        // waiting for the run loop to report — meant the UI toggle visibly
        // snapped back to "Pause" while a run was in flight.
        let mut status = self.status_rx.borrow().clone();
        status.paused = paused;
        status.state = if paused { SyncState::Paused } else { SyncState::Idle };
        if !paused {
            status.message = None;
        }
        let _ = self.status_tx.send(status.clone());
        let _ = self.app.emit("sync://status", &status);
        self.kick(); // resume immediately if unpaused; refresh otherwise
    }

    /// Flip a folder's enabled flag for the *running* engine: an in-flight sync
    /// of that folder is cancelled, not just future rounds.
    pub fn set_folder_enabled(&self, id: &str, enabled: bool) {
        if let Ok(mut d) = self.disabled.write() {
            if enabled {
                d.remove(id);
            } else {
                d.insert(id.to_string());
            }
        }
        self.kick();
    }
}

/// The background loop: run on startup, every 60s, on explicit triggers, and on
/// local filesystem changes (debounced).
async fn run_loop(
    app: AppHandle,
    mut trigger_rx: mpsc::UnboundedReceiver<()>,
    status_tx: watch::Sender<SyncStatus>,
    reporter: Reporter,
    paused: Arc<AtomicBool>,
    disabled: DisabledFolders,
    online: Arc<AtomicBool>,
) {
    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel();
    let mut _watcher = rewatch(&app, &fs_tx);
    let mut interval = tokio::time::interval(Duration::from_secs(REMOTE_POLL_SECS));
    // Default `Burst` behaviour would fire every *missed* tick back-to-back the
    // moment a long run finishes (a multi-GB sync overruns the period many times
    // over), producing a storm of immediate rescans. `Delay` fires at most one
    // catch-up tick and then waits a full period again.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = trigger_rx.recv() => {}
            _ = fs_rx.recv() => {
                // Coalesce bursts of filesystem events into one run.
                tokio::time::sleep(Duration::from_millis(800)).await;
                while fs_rx.try_recv().is_ok() {}
            }
        }

        // Panic shield: a panicking run must not kill this loop — that left
        // the app silently never syncing again until restart. The panic is
        // reported as a sync error instead, and the loop lives on.
        let run = std::panic::AssertUnwindSafe(run_all(
            &app,
            &status_tx,
            &reporter,
            &paused,
            &disabled,
            &online,
        ));
        if let Err(panic) = futures_util::FutureExt::catch_unwind(run).await {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            log::error!("sync run panicked: {msg}");
            reporter.session_end();
            let count = AppConfig::load(&app)
                .map(|c| c.sync_folders.iter().filter(|f| f.enabled).count())
                .unwrap_or(0);
            publish(
                &app,
                &status_tx,
                SyncState::Error,
                None,
                Some(format!("internal sync error: {msg}")),
                count,
                false,
            );
        }
        // Folders may have been added/removed while we ran.
        _watcher = rewatch(&app, &fs_tx);
        // Re-arm the periodic poll to a full period *after* the run finishes, so
        // a trigger/fs-event run (or a long run) doesn't leave the timer due to
        // fire again immediately. Local edits are caught instantly by the
        // watcher; this periodic pass only exists to notice *remote* changes.
        interval.reset();
    }
}

/// How often to poll for remote-side changes when idle. Local changes are
/// picked up immediately via the filesystem watcher, so this can be relaxed —
/// a tight interval just re-walks every folder (recursive PROPFIND + local
/// hashing) for nothing and keeps flipping the tray between idle/syncing.
const REMOTE_POLL_SECS: u64 = 300;

/// How often the watchdog checks that the server is still there. Much tighter
/// than [`REMOTE_POLL_SECS`] because it costs one tiny Depth-0 PROPFIND, and
/// because "the tray says synced but the link is dead" is exactly the lie this
/// exists to prevent.
const HEALTH_POLL_SECS: u64 = 30;

/// Shown whenever the server can't be reached; also used as the tray tooltip.
const OFFLINE_MESSAGE: &str = "server unreachable — check your connection";

/// Whether *any* connected account answers right now.
///
/// `None` means there is nothing to probe (no account is signed in) — a
/// different condition from "signed in but unreachable", and the caller must
/// not mistake one for the other.
async fn any_reachable(app: &AppHandle) -> Option<bool> {
    let state = app.state::<AppState>();
    let accounts = state.accounts().await;
    if accounts.is_empty() {
        return None;
    }
    for account in &accounts {
        let Ok(client) = state.client_for(&account.id).await else {
            continue;
        };
        match client.probe().await {
            Ok(()) => return Some(true),
            Err(e) => log::debug!("probe failed for {}: {e}", account.id),
        }
    }
    Some(false)
}

/// Watch the link to the server independently of the sync loop.
///
/// Without this, connectivity was only ever discovered as a side effect of a
/// sync run — so a link that died while idle left the tray green and the UI
/// claiming "up to date" until the next 5-minute poll finally stalled inside a
/// request. The watchdog reports the loss within [`HEALTH_POLL_SECS`], and
/// kicks a sync the moment the server answers again.
async fn health_loop(
    app: AppHandle,
    status_tx: watch::Sender<SyncStatus>,
    trigger: mpsc::UnboundedSender<()>,
    paused: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(HEALTH_POLL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        if paused.load(Ordering::Relaxed) {
            continue;
        }
        // Not signed in: leave the state alone, the sync loop already reports it.
        let Some(reachable) = any_reachable(&app).await else {
            continue;
        };
        if online.swap(reachable, Ordering::Relaxed) == reachable {
            continue; // no transition
        }
        if reachable {
            log::info!("server reachable again — resuming sync");
            let _ = trigger.send(());
        } else {
            log::warn!("server unreachable — sync is offline");
            let count = AppConfig::load(&app)
                .map(|c| c.sync_folders.iter().filter(|f| f.enabled).count())
                .unwrap_or(0);
            publish(
                &app,
                &status_tx,
                SyncState::Offline,
                None,
                Some(OFFLINE_MESSAGE.to_string()),
                count,
                false,
            );
        }
    }
}

async fn run_all(
    app: &AppHandle,
    status_tx: &watch::Sender<SyncStatus>,
    reporter: &Reporter,
    paused: &Arc<AtomicBool>,
    disabled: &DisabledFolders,
    online: &AtomicBool,
) {
    let cfg = match AppConfig::load(app) {
        Ok(c) => c,
        Err(_) => return,
    };
    let ignore = cfg.ignore_patterns.clone();
    let folders: Vec<SyncFolder> = cfg.sync_folders.into_iter().filter(|f| f.enabled).collect();
    let count = folders.len();

    if paused.load(Ordering::Relaxed) {
        publish(app, status_tx, SyncState::Paused, None, None, count, true);
        return;
    }

    // Cancellation for one folder: the global pause OR that folder being
    // disabled mid-run. Checked live inside the engine at every safe point.
    let folder_cancel = |id: &str| {
        let paused = paused.clone();
        let disabled = disabled.clone();
        let id = id.to_string();
        Cancel::new(move || {
            paused.load(Ordering::Relaxed)
                || disabled.read().map(|d| d.contains(&id)).unwrap_or(false)
        })
    };
    let folder_disabled =
        |id: &str| disabled.read().map(|d| d.contains(id)).unwrap_or(false);

    // Reachability gate. A signed-in account whose server doesn't answer used to
    // fall straight through into the walk, where the run sat inside a request
    // until it eventually timed out — the tray still green, "Sync now" seemingly
    // hung. Probing first (10s cap) turns that into an immediate, honest Offline.
    match any_reachable(app).await {
        Some(true) => online.store(true, Ordering::Relaxed),
        Some(false) => {
            online.store(false, Ordering::Relaxed);
            publish(
                app,
                status_tx,
                SyncState::Offline,
                None,
                Some(OFFLINE_MESSAGE.to_string()),
                count,
                false,
            );
            return;
        }
        // No account signed in. There is nothing to sync and nothing has been
        // contacted, so the run must not fall through to the "finished with no
        // errors" path at the bottom — that publishes Idle, i.e. a green tray
        // reading "up to date" on a machine that has never reached a server.
        None => {
            publish(app, status_tx, SyncState::Offline, None, None, count, false);
            return;
        }
    }

    let state = app.state::<AppState>();
    let jdir = journals_dir(app);
    let was_error = status_tx.borrow().state == SyncState::Error;
    reporter.session_reset();
    let mut last_error: Option<String> = None;
    let mut new_conflicts = 0u32;
    let mut any_offline = false;

    // Phase 1 — prepare every folder against its account's client (walk + plan)
    // so the UI gets stable whole-run totals before the first byte transfers.
    let mut prepared: Vec<(&SyncFolder, crate::webdav::WebDavClient, engine::Prepared)> =
        Vec::new();
    let (mut files_total, mut bytes_total, mut verify_total) = (0u64, 0u64, 0u64);
    for folder in &folders {
        if paused.load(Ordering::Relaxed) {
            publish(app, status_tx, SyncState::Paused, None, None, count, true);
            return;
        }
        if folder_disabled(&folder.id) {
            continue;
        }
        let client = match state.client_for(&folder.account_id).await {
            Ok(c) => c,
            Err(_) => {
                // The account this folder belongs to isn't connected right now.
                any_offline = true;
                continue;
            }
        };
        let cancel = folder_cancel(&folder.id);
        // NB: no `Syncing` status here. The scan is silent so an idle poll that
        // finds nothing to do never flips the tray green→blue→green. We only go
        // `Syncing` in phase 2, and only for folders that actually transfer.
        match engine::prepare(&jdir, &client, folder, &ignore, reporter, &cancel).await {
            Ok(p) => {
                files_total += p.files_total;
                bytes_total += p.bytes_total;
                verify_total += p.verify_files;
                prepared.push((folder, client, p));
            }
            // A cancelled scan is not a sync error — the pause/disable check
            // at the top of the next iteration (or round) reports the state.
            Err(_) if cancel.is_cancelled() => {}
            Err(e) => {
                log::warn!("sync scan failed for {}: {e}", folder.remote_path);
                last_error = Some(e.to_string());
            }
        }
    }

    // No account reachable at all → offline, nothing to do this round.
    if prepared.is_empty() && last_error.is_none() && any_offline {
        publish(app, status_tx, SyncState::Offline, None, None, count, false);
        return;
    }
    reporter.session_plan(files_total, bytes_total, verify_total);

    // Phase 2 — execute the transfers. Only surface `Syncing` for folders that
    // actually have work, so a fully-synced run stays visually idle (no flash).
    for (folder, client, plan) in prepared {
        if paused.load(Ordering::Relaxed) {
            break;
        }
        if folder_disabled(&folder.id) {
            continue;
        }
        // Verification-only folders still surface `Syncing` — comparing may
        // stream bytes for a while, and a green "up to date" would be a lie.
        if plan.files_total > 0 || plan.verify_files > 0 {
            publish(app, status_tx, SyncState::Syncing, Some(folder.remote_path.clone()), None, count, false);
        }
        let cancel = folder_cancel(&folder.id);
        match engine::sync_prepared(&jdir, &client, folder, plan, reporter, &ignore, &cancel).await
        {
            Ok(stats) => {
                new_conflicts += stats.conflicts;
                if stats.blocked_deletions > 0 {
                    last_error = Some(format!(
                        "{}: refused to delete {} files — one side looked missing or emptied; \
                         the files were kept and will be restored by the next sync",
                        folder.remote_path, stats.blocked_deletions
                    ));
                }
                log::info!("synced {}: {:?}", folder.remote_path, stats);
            }
            Err(_) if cancel.is_cancelled() => {}
            Err(e) => {
                log::warn!("sync failed for {}: {e}", folder.remote_path);
                last_error = Some(e.to_string());
            }
        }
    }
    reporter.session_end();

    // Paused mid-run: report it now (partial work was journaled safely) and
    // keep the previous last-sync timestamp — this round did not complete.
    if paused.load(Ordering::Relaxed) {
        publish(app, status_tx, SyncState::Paused, None, None, count, true);
        return;
    }

    // Desktop notifications: only on state transitions / fresh conflicts so a
    // persistent error doesn't re-notify every scheduled run.
    if let Some(err) = &last_error {
        if !was_error {
            notify(app, "Sync error", &format!("Syncing failed: {err}"));
        }
    }
    if new_conflicts > 0 {
        notify(
            app,
            "Sync conflicts",
            &format!(
                "{new_conflicts} file{} had conflicting changes — review them in Synced folders.",
                if new_conflicts == 1 { "" } else { "s" }
            ),
        );
    }

    let state = if last_error.is_some() {
        SyncState::Error
    } else {
        SyncState::Idle
    };
    let mut status = SyncStatus::new(count);
    status.state = state;
    status.message = last_error;
    status.last_sync = Some(now_rfc3339());
    let _ = status_tx.send(status.clone());
    let _ = app.emit("sync://status", &status);
}

#[allow(clippy::too_many_arguments)]
fn publish(
    app: &AppHandle,
    status_tx: &watch::Sender<SyncStatus>,
    state: SyncState,
    active: Option<String>,
    message: Option<String>,
    count: usize,
    paused: bool,
) {
    let mut status = SyncStatus::new(count);
    status.state = state;
    status.active_folder = active;
    status.message = message;
    status.paused = paused;
    // Interim states (paused/offline/syncing) don't erase when the folders
    // last finished a full round — only a completed round updates it.
    status.last_sync = status_tx.borrow().last_sync.clone();
    let _ = status_tx.send(status.clone());
    let _ = app.emit("sync://status", &status);
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::debug!("notification failed: {e}");
    }
}

/// Build a fresh watcher over all enabled folders. Returns `None` if it can't be
/// created; the periodic scan still keeps things in sync in that case.
fn rewatch(app: &AppHandle, fs_tx: &mpsc::UnboundedSender<()>) -> Option<RecommendedWatcher> {
    let tx = fs_tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()?;

    if let Ok(cfg) = AppConfig::load(app) {
        for folder in cfg.sync_folders.iter().filter(|f| f.enabled) {
            let _ = watcher.watch(Path::new(&folder.local_path), RecursiveMode::Recursive);
        }
    }
    Some(watcher)
}

/// Directory where per-folder journals live (under the app data dir).
fn journals_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .map(|d| d.join("journals"))
        .unwrap_or_else(|_| std::env::temp_dir().join("cirrust-journals"))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Do two folder paths overlap — equal, or one nested inside the other —
/// compared by whole path segments (`/a/bc` does not overlap `/a/b`)?
///
/// Overlapping pairs are refused at add time: two pairs over the same remote
/// tree download everything twice and then fight each other's deletions, and
/// two pairs over the same local tree race the filesystem watcher. This
/// invariant lives in the backend so no UI rework can lose it.
fn paths_overlap(a: &str, b: &str) -> bool {
    let norm = |p: &str| format!("{}/", p.trim_end_matches('/'));
    let (a, b) = (norm(a), norm(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("f{nanos:x}")
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn sync_list_folders(app: AppHandle) -> AppResult<Vec<SyncFolder>> {
    Ok(AppConfig::load(&app)?.sync_folders)
}

/// Per-folder metadata for the folder list: how much is tracked and when it
/// last synced (the journal file's mtime).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStat {
    pub id: String,
    pub files: u64,
    pub bytes: u64,
    pub last_sync: Option<String>,
}

#[tauri::command]
pub fn sync_folder_stats(app: AppHandle) -> AppResult<Vec<FolderStat>> {
    let cfg = AppConfig::load(&app)?;
    let dir = journals_dir(&app);
    let mut out = Vec::new();
    for f in &cfg.sync_folders {
        let journal = Journal::load(&dir, &f.id).unwrap_or_default();
        let (files, bytes) = journal
            .entries
            .values()
            .filter(|e| !e.is_dir)
            .fold((0u64, 0u64), |(c, b), e| (c + 1, b + e.size));
        let last_sync = std::fs::metadata(dir.join(format!("{}.json", f.id)))
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
        out.push(FolderStat { id: f.id.clone(), files, bytes, last_sync });
    }
    Ok(out)
}

#[tauri::command]
pub async fn sync_add_folder(
    app: AppHandle,
    manager: State<'_, SyncManager>,
    state: State<'_, AppState>,
    account_id: Option<String>,
    local_path: String,
    remote_path: String,
    merge_existing: Option<bool>,
) -> AppResult<SyncFolder> {
    let mut cfg = AppConfig::load(&app)?;
    // Default to the active account when the caller doesn't specify one.
    let account_id = account_id
        .or_else(|| cfg.active_account.clone())
        .or_else(|| cfg.accounts.first().map(|a| a.id.clone()))
        .ok_or_else(|| AppError::msg("no account to sync against"))?;
    // A local-first add must never absorb pre-existing server data: unless the
    // caller explicitly picked an existing server folder (`merge_existing`),
    // an occupied remote name becomes a second version — "<name> 2" — so the
    // sync cannot merge with (or ever delete) files it didn't create.
    let requested = format!("/{}", remote_path.trim_matches('/'));
    let remote_path = if merge_existing.unwrap_or(false) {
        requested
    } else {
        let client = state.client_for(&account_id).await?;
        engine::unique_remote_path(&client, &requested).await?
    };
    // Refuse overlapping pairs (see `paths_overlap`): the same remote tree
    // twice means duplicate full downloads + fighting deletions; the same
    // local tree twice means two engines writing into each other.
    for f in &cfg.sync_folders {
        if f.account_id == account_id && paths_overlap(&f.remote_path, &remote_path) {
            return Err(AppError::msg(format!(
                "{remote_path} is already being synced to {} — remove that folder pair first, \
                 or choose a different server folder",
                f.local_path
            )));
        }
        if paths_overlap(&f.local_path, &local_path) {
            return Err(AppError::msg(format!(
                "{local_path} overlaps the synced folder {} — choose a different local folder",
                f.local_path
            )));
        }
    }
    let folder = SyncFolder {
        id: new_id(),
        account_id,
        local_path,
        remote_path,
        enabled: true,
    };
    cfg.sync_folders.push(folder.clone());
    cfg.save(&app)?;
    manager.kick();
    Ok(folder)
}

#[tauri::command]
pub async fn sync_remove_folder(
    app: AppHandle,
    manager: State<'_, SyncManager>,
    id: String,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app)?;
    cfg.sync_folders.retain(|f| f.id != id);
    cfg.save(&app)?;
    // Cancel any in-flight sync of the folder (ids are never reused, so the
    // entry staying in the disabled set is harmless), then drop its journal.
    manager.set_folder_enabled(&id, false);
    Journal::delete(&journals_dir(&app), &id);
    manager.kick();
    Ok(())
}

#[tauri::command]
pub fn sync_status(manager: State<'_, SyncManager>) -> SyncStatus {
    manager.status()
}

#[tauri::command]
pub fn sync_progress(manager: State<'_, SyncManager>) -> Progress {
    manager.progress()
}

#[tauri::command]
pub fn sync_activity(manager: State<'_, SyncManager>) -> Vec<Activity> {
    manager.activity()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSettings {
    pub paused: bool,
    pub ignore_patterns: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub folder_id: String,
    pub folder_remote: String,
    /// Absolute local path of the conflicted-copy file.
    pub local_path: String,
    pub name: String,
    pub original_name: String,
}

#[tauri::command]
pub fn sync_set_paused(
    app: AppHandle,
    manager: State<'_, SyncManager>,
    paused: bool,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app)?;
    cfg.paused = paused;
    cfg.save(&app)?;
    manager.set_paused(paused);
    Ok(())
}

#[tauri::command]
pub async fn sync_set_folder_enabled(
    app: AppHandle,
    manager: State<'_, SyncManager>,
    id: String,
    enabled: bool,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app)?;
    let folder = cfg
        .sync_folders
        .iter_mut()
        .find(|f| f.id == id)
        .ok_or_else(|| AppError::msg("unknown sync folder"))?;
    folder.enabled = enabled;
    cfg.save(&app)?;
    // Mirrors the config change into the live engine — this is what cancels an
    // in-flight sync of the folder instead of letting it run to completion.
    manager.set_folder_enabled(&id, enabled);
    Ok(())
}

#[tauri::command]
pub fn sync_settings(app: AppHandle) -> AppResult<SyncSettings> {
    let cfg = AppConfig::load(&app)?;
    Ok(SyncSettings { paused: cfg.paused, ignore_patterns: cfg.ignore_patterns })
}

#[tauri::command]
pub async fn sync_set_ignore_patterns(
    app: AppHandle,
    manager: State<'_, SyncManager>,
    patterns: Vec<String>,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app)?;
    cfg.ignore_patterns = patterns
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    cfg.save(&app)?;
    manager.kick();
    Ok(())
}

/// Walk synced folders and collect leftover "conflicted copy" files.
fn scan_conflicts(cfg: &AppConfig) -> Vec<Conflict> {
    let mut out = Vec::new();
    for folder in &cfg.sync_folders {
        for entry in WalkDir::new(&folder.local_path)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(original_name) = engine::conflict_original(&name) {
                out.push(Conflict {
                    folder_id: folder.id.clone(),
                    folder_remote: folder.remote_path.clone(),
                    local_path: entry.path().to_string_lossy().into_owned(),
                    name,
                    original_name,
                });
            }
        }
    }
    out
}

/// Scan synced folders for leftover "conflicted copy" files.
#[tauri::command]
pub fn sync_conflicts(app: AppHandle) -> AppResult<Vec<Conflict>> {
    Ok(scan_conflicts(&AppConfig::load(&app)?))
}

/// Delete every conflicted-copy file whose content is byte-identical to its
/// original (spurious conflicts, e.g. from first syncs before adoption
/// existed). Returns how many copies were removed.
#[tauri::command]
pub async fn sync_dismiss_identical_conflicts(
    app: AppHandle,
    manager: State<'_, SyncManager>,
) -> AppResult<u32> {
    let cfg = AppConfig::load(&app)?;
    let mut dismissed = 0u32;
    for c in scan_conflicts(&cfg) {
        let copy = PathBuf::from(&c.local_path);
        let original = copy.with_file_name(&c.original_name);
        let (Ok(copy_md), Ok(orig_md)) = (copy.metadata(), original.metadata()) else {
            continue;
        };
        if copy_md.len() != orig_md.len() {
            continue;
        }
        let (Ok(a), Ok(b)) = (tokio::fs::read(&copy).await, tokio::fs::read(&original).await)
        else {
            continue;
        };
        if a == b && tokio::fs::remove_file(&copy).await.is_ok() {
            dismissed += 1;
        }
    }
    if dismissed > 0 {
        manager.kick();
    }
    Ok(dismissed)
}

/// Resolve a conflict: `keep = "local"` replaces the original with the local
/// edit (uploaded next sync); `keep = "remote"` discards the local copy.
#[tauri::command]
pub async fn sync_resolve_conflict(
    manager: State<'_, SyncManager>,
    local_path: String,
    keep: String,
) -> AppResult<()> {
    let path = PathBuf::from(&local_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let original = engine::conflict_original(&name)
        .ok_or_else(|| AppError::msg("not a conflicted-copy file"))?;

    match keep.as_str() {
        "local" => {
            tokio::fs::rename(&path, path.with_file_name(&original)).await?;
        }
        "remote" => {
            tokio::fs::remove_file(&path).await?;
        }
        _ => return Err(AppError::msg("keep must be 'local' or 'remote'")),
    }
    manager.kick();
    Ok(())
}

#[tauri::command]
pub fn sync_now(manager: State<'_, SyncManager>) -> AppResult<()> {
    manager.kick();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::paths_overlap;

    #[test]
    fn overlap_is_segment_wise() {
        assert!(paths_overlap("/Music", "/Music"), "equal paths overlap");
        assert!(paths_overlap("/Music", "/Music/sub"), "parent covers child");
        assert!(paths_overlap("/Music/sub", "/Music"), "child is covered by parent");
        assert!(paths_overlap("/", "/anything"), "root covers everything");
        assert!(!paths_overlap("/Music", "/Music 2"), "sibling with a name prefix is distinct");
        assert!(!paths_overlap("/Music", "/Musical"), "string prefix is not a path prefix");
        assert!(!paths_overlap("/a/b", "/a/c"), "siblings are distinct");
        assert!(paths_overlap("/home/x/Cloud", "/home/x/Cloud/music"), "local nesting too");
    }
}
