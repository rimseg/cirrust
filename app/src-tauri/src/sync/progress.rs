//! Live sync progress + activity reporting.
//!
//! The engine emits fine-grained [`SyncEvent`]s through a [`Reporter`]. The
//! [`consume`] task folds those into a single [`Progress`] snapshot (current
//! file, files/bytes done vs. total, transfer speed) published on a `watch`
//! channel + the `sync://progress` Tauri event, and appends human-readable
//! entries to a capped [`Activity`] log.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch};

const ACTIVITY_CAP: usize = 200;
/// UI update cadence. Half a second keeps the display readable; the speed is
/// additionally smoothed with an exponential moving average.
const TICK: Duration = Duration::from_millis(500);
/// EMA weight of the newest speed sample (0..1); lower = smoother.
const SPEED_ALPHA: f64 = 0.35;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Upload,
    Download,
    /// Byte-comparison of same-size files on both sides. Concurrent
    /// verification reports via `VerifyDone` instead; kept for sequential use.
    #[allow(dead_code)]
    Verify,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Upload => "upload",
            Direction::Download => "download",
            Direction::Verify => "verify",
        }
    }
}

/// Internal event stream from the engine to the aggregator.
#[derive(Debug)]
pub enum SyncEvent {
    /// Start of a run — clears the previous snapshot.
    SessionReset,
    /// Scan-phase progress: entries discovered so far in `folder`.
    ScanProgress { folder: String, entries: u64 },
    /// Additive plan for a folder about to sync.
    SessionPlan { files: u64, bytes: u64 },
    /// End of a run.
    SessionEnd,
    FileStart { path: String, dir: Direction, total: u64 },
    FileProgress { path: String, done: u64, total: u64 },
    FileDone { path: String, dir: Direction, size: u64 },
    /// A transfer failed/gave up — drop it from the in-flight set (no completion).
    FileAborted { path: String },
    /// A file was verified identical on both sides (runs concurrently, so it
    /// carries its own byte accounting instead of the FileStart/Done pair).
    VerifyDone { path: String, size: u64 },
    Deleted { path: String, remote: bool },
    Conflict { path: String },
    Error { path: String, message: String },
}

/// Cloneable handle the engine uses to report progress.
#[derive(Clone)]
pub struct Reporter {
    tx: mpsc::UnboundedSender<SyncEvent>,
}

impl Reporter {
    pub fn new(tx: mpsc::UnboundedSender<SyncEvent>) -> Self {
        Reporter { tx }
    }
    fn send(&self, e: SyncEvent) {
        let _ = self.tx.send(e);
    }
    pub fn session_reset(&self) {
        self.send(SyncEvent::SessionReset);
    }
    pub fn scan_progress(&self, folder: &str, entries: u64) {
        self.send(SyncEvent::ScanProgress { folder: folder.into(), entries });
    }
    pub fn session_plan(&self, files: u64, bytes: u64) {
        self.send(SyncEvent::SessionPlan { files, bytes });
    }
    pub fn session_end(&self) {
        self.send(SyncEvent::SessionEnd);
    }
    pub fn file_start(&self, path: &str, dir: Direction, total: u64) {
        self.send(SyncEvent::FileStart { path: path.into(), dir, total });
    }
    pub fn file_progress(&self, path: &str, done: u64, total: u64) {
        self.send(SyncEvent::FileProgress { path: path.into(), done, total });
    }
    pub fn file_done(&self, path: &str, dir: Direction, size: u64) {
        self.send(SyncEvent::FileDone { path: path.into(), dir, size });
    }
    pub fn file_aborted(&self, path: &str) {
        self.send(SyncEvent::FileAborted { path: path.into() });
    }
    pub fn verify_done(&self, path: &str, size: u64) {
        self.send(SyncEvent::VerifyDone { path: path.into(), size });
    }
    pub fn deleted(&self, path: &str, remote: bool) {
        self.send(SyncEvent::Deleted { path: path.into(), remote });
    }
    pub fn conflict(&self, path: &str) {
        self.send(SyncEvent::Conflict { path: path.into() });
    }
    pub fn error(&self, path: &str, message: &str) {
        self.send(SyncEvent::Error { path: path.into(), message: message.into() });
    }
}

/// One file currently being transferred (there can be several at once).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveFile {
    pub path: String,
    /// "upload" | "download".
    pub direction: String,
    pub done: u64,
    pub total: u64,
}

/// Live snapshot of an in-flight (or just-finished) sync run.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub active: bool,
    /// "scanning" while folders are walked/planned, "transferring" afterwards.
    pub phase: String,
    /// Remote entries discovered so far in the folder being scanned.
    pub scanned: u64,
    /// Folder currently being scanned (scan phase only).
    pub current_file: String,
    /// Every file in flight right now — one row per concurrent transfer.
    pub active_files: Vec<ActiveFile>,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes per second (exponentially smoothed).
    pub speed: u64,
    /// Estimated seconds until the run finishes, when computable.
    pub eta_secs: Option<u64>,
}

/// One entry in the recent-activity log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub time: String,
    /// uploaded | downloaded | deleted | conflict | error
    pub kind: String,
    pub path: String,
    pub size: u64,
    pub message: Option<String>,
}

pub type ActivityLog = Arc<Mutex<VecDeque<Activity>>>;

fn push_activity(log: &ActivityLog, kind: &str, path: &str, size: u64, message: Option<String>) {
    if let Ok(mut q) = log.lock() {
        if q.len() >= ACTIVITY_CAP {
            q.pop_front();
        }
        q.push_back(Activity {
            time: chrono::Utc::now().to_rfc3339(),
            kind: kind.into(),
            path: path.into(),
            size,
            message,
        });
    }
}

/// Fold engine events into `progress_tx` + `activity`, emitting throttled
/// `sync://progress` updates. Runs for the app's lifetime.
pub async fn consume(
    app: AppHandle,
    mut rx: mpsc::UnboundedReceiver<SyncEvent>,
    progress_tx: watch::Sender<Progress>,
    activity: ActivityLog,
) {
    let mut p = Progress::default();
    let mut last_bytes = 0u64; // bytes_done at previous tick, for speed
    // Files currently in flight (one entry per concurrent transfer). Also
    // drives correct per-file byte accounting into the global total.
    let mut active: HashMap<String, ActiveFile> = HashMap::new();
    let mut speed_ema = 0f64; // smoothed bytes/sec
    let mut interval = tokio::time::interval(TICK);

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                let Some(ev) = maybe else { break };
                match ev {
                    SyncEvent::SessionReset => {
                        p = Progress {
                            active: true,
                            phase: "scanning".into(),
                            ..Default::default()
                        };
                        last_bytes = 0;
                        active.clear();
                        speed_ema = 0.0;
                    }
                    SyncEvent::ScanProgress { folder, entries } => {
                        p.active = true;
                        p.phase = "scanning".into();
                        p.current_file = folder;
                        p.scanned = entries;
                    }
                    SyncEvent::SessionPlan { files, bytes } => {
                        p.active = true;
                        p.phase = "transferring".into();
                        p.current_file.clear();
                        p.files_total += files;
                        p.bytes_total += bytes;
                    }
                    SyncEvent::SessionEnd => {
                        p.active = false;
                        p.phase.clear();
                        p.scanned = 0;
                        p.current_file.clear();
                        p.active_files.clear();
                        p.speed = 0;
                        p.eta_secs = None;
                        active.clear();
                        speed_ema = 0.0;
                    }
                    SyncEvent::FileStart { path, dir, total } => {
                        active.insert(
                            path.clone(),
                            ActiveFile { path, direction: dir.as_str().into(), done: 0, total },
                        );
                    }
                    SyncEvent::FileProgress { path, done, total } => {
                        // Add only the newly-transferred bytes to the global
                        // total (handles concurrent files correctly).
                        let entry = active.entry(path.clone()).or_insert(ActiveFile {
                            path,
                            direction: "download".into(),
                            done: 0,
                            total,
                        });
                        p.bytes_done += done.saturating_sub(entry.done);
                        entry.done = done;
                        entry.total = total;
                    }
                    SyncEvent::FileDone { path, dir, size } => {
                        p.files_done += 1;
                        // True the file up to its full size, then forget it.
                        let seen = active.remove(&path).map_or(0, |a| a.done);
                        p.bytes_done += size.saturating_sub(seen);
                        push_activity(
                            &activity,
                            match dir {
                                Direction::Upload => "uploaded",
                                Direction::Download => "downloaded",
                                Direction::Verify => "verified",
                            },
                            &path,
                            size,
                            None,
                        );
                    }
                    SyncEvent::VerifyDone { path, size } => {
                        p.files_done += 1;
                        p.bytes_done += size;
                        push_activity(&activity, "verified", &path, size, None);
                    }
                    SyncEvent::FileAborted { path } => {
                        // Drop the failed transfer's partial bytes and remove
                        // it from the in-flight set (it did not complete).
                        if let Some(a) = active.remove(&path) {
                            p.bytes_done = p.bytes_done.saturating_sub(a.done);
                        }
                    }
                    SyncEvent::Deleted { path, remote } => {
                        push_activity(
                            &activity,
                            "deleted",
                            &path,
                            0,
                            Some(if remote { "on server".into() } else { "locally".into() }),
                        );
                    }
                    SyncEvent::Conflict { path } => {
                        push_activity(&activity, "conflict", &path, 0, None);
                    }
                    SyncEvent::Error { path, message } => {
                        push_activity(&activity, "error", &path, 0, Some(message));
                    }
                }
            }
            _ = interval.tick() => {
                let delta = p.bytes_done.saturating_sub(last_bytes);
                last_bytes = p.bytes_done;
                if p.active {
                    let instant = delta as f64 * (1000.0 / TICK.as_millis() as f64);
                    speed_ema = SPEED_ALPHA * instant + (1.0 - SPEED_ALPHA) * speed_ema;
                    p.speed = speed_ema as u64;
                    p.eta_secs = if p.speed > 1024 && p.bytes_total > p.bytes_done {
                        Some((p.bytes_total - p.bytes_done) / p.speed)
                    } else {
                        None
                    };
                } else {
                    p.speed = 0;
                    p.eta_secs = None;
                }
                // Snapshot the in-flight files, ordered stably by path.
                let mut files: Vec<ActiveFile> = active.values().cloned().collect();
                files.sort_by(|a, b| a.path.cmp(&b.path));
                p.active_files = files;

                // Emit while active, plus once on the transition to idle.
                if p.active || progress_tx.borrow().active {
                    let _ = progress_tx.send(p.clone());
                    let _ = app.emit("sync://progress", &p);
                }
            }
        }
    }
}
