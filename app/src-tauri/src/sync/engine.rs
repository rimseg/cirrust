//! Three-way bidirectional reconciliation for a single folder pair.
//!
//! For every path we compare three states — remote (WebDAV), local (disk) and
//! the journal (last-synced base) — to classify the change and apply it:
//!
//! | remote | local | journal            | action                         |
//! |--------|-------|--------------------|--------------------------------|
//! | new/chg| gone  | unchanged remote   | download / delete-remote       |
//! | gone   | new/chg| unchanged local   | delete-local / upload          |
//! | changed| changed| —                 | conflict → keep both           |
//!
//! Directory creation is applied parent-first; deletions child-first.

use super::journal::{Journal, JournalEntry};
use super::progress::{Direction, Reporter};
use crate::config::SyncFolder;
use crate::error::{AppError, AppResult};
use crate::webdav::WebDavClient;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Cooperative cancellation for a running sync (global pause or a per-folder
/// disable). Checked at every safe point: between directory listings during the
/// walk, per path while planning, before/around each transfer, and per
/// deletion — so a pause takes effect within seconds instead of after the run.
#[derive(Clone)]
pub struct Cancel(Arc<dyn Fn() -> bool + Send + Sync>);

impl Cancel {
    pub fn new(f: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Cancel(Arc::new(f))
    }

    /// A handle that never cancels (tests, one-shot syncs).
    pub fn never() -> Self {
        Cancel(Arc::new(|| false))
    }

    pub fn is_cancelled(&self) -> bool {
        (self.0)()
    }

    /// Resolves once cancellation is requested — for racing against an
    /// in-flight transfer with `select!`. Polling (vs. a wakeup channel) keeps
    /// the flag composable from plain atomics; 250ms is imperceptible next to
    /// any network transfer.
    async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

/// The action chosen for a path after comparing remote / local / journal.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Decision {
    None,
    Download,
    Upload,
    Conflict,
    MkdirLocal,
    MkdirRemote,
    DeleteLocal,
    DeleteRemote,
}

fn classify(r: Option<&RemoteMeta>, l: Option<&LocalMeta>, j: Option<&JournalEntry>) -> Decision {
    match (r, l) {
        (Some(r), Some(l)) => {
            if r.is_dir && l.is_dir {
                return Decision::None;
            }
            if r.is_dir != l.is_dir {
                return Decision::None; // type changed; leave as-is
            }
            let remote_changed = j.map_or(true, |j| j.etag != r.etag);
            let local_changed = j.map_or(true, |j| j.size != l.size || j.local_mtime != l.mtime);
            match (remote_changed, local_changed) {
                (true, true) => Decision::Conflict,
                (true, false) => Decision::Download,
                (false, true) => Decision::Upload,
                (false, false) => Decision::None,
            }
        }
        (Some(r), None) => {
            if r.is_dir {
                // A directory's ETag changes whenever its children change, so it
                // can't distinguish "modified" from "same folder, different
                // contents" — comparing it made a folder that ever held files
                // look changed, so a local deletion re-created it instead of
                // removing it. Decide by whether we knew the folder: known and
                // gone locally = the user deleted it → remove it remotely
                // (execution only removes it once empty, so a child added
                // remotely is preserved); unknown = a remote addition → mirror.
                if j.map_or(false, |j| j.is_dir) {
                    Decision::DeleteRemote
                } else {
                    Decision::MkdirLocal
                }
            } else {
                let unchanged = j.map_or(false, |j| j.etag == r.etag && !j.is_dir);
                if unchanged {
                    Decision::DeleteRemote
                } else {
                    Decision::Download
                }
            }
        }
        (None, Some(l)) => {
            if l.is_dir {
                if j.map_or(false, |j| j.is_dir) {
                    Decision::DeleteLocal
                } else {
                    Decision::MkdirRemote
                }
            } else {
                let unchanged =
                    j.map_or(false, |j| j.size == l.size && j.local_mtime == l.mtime && !j.is_dir);
                if unchanged {
                    Decision::DeleteLocal
                } else {
                    Decision::Upload
                }
            }
        }
        (None, None) => Decision::None,
    }
}

/// Whether a remote directory has no children. `list` (PROPFIND depth 1) already
/// excludes the collection itself, so an empty folder yields no entries. On a
/// listing error we answer `false` — better to leave a folder in place than to
/// risk a recursive delete of something we couldn't see.
async fn remote_dir_empty(client: &WebDavClient, path: &str) -> bool {
    matches!(client.list(path).await, Ok(entries) if entries.is_empty())
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncStats {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted_local: u32,
    pub deleted_remote: u32,
    pub conflicts: u32,
    /// Deletions refused by the mass-deletion guard (see [`sync_prepared`]).
    pub blocked_deletions: u32,
}

struct RemoteMeta {
    is_dir: bool,
    size: u64,
    etag: Option<String>,
    checksums: Option<String>,
}

struct LocalMeta {
    is_dir: bool,
    size: u64,
    mtime: i64,
}

/// A same-size both-sides file queued for concurrent verification.
struct VerifyItem {
    key: String,
    remote_full: String,
    local_full: PathBuf,
    size: u64,
    mtime: i64,
    etag: Option<String>,
    checksums: Option<String>,
}

/// How many files are verified (hashed/compared) in parallel.
const VERIFY_CONCURRENCY: usize = 6;

/// Permit budget for concurrent uploads/downloads. Network transfers are
/// I/O-bound, so concurrency on one thread hides per-request latency (the
/// gap between files) without needing extra OS threads. Up to 8 small files
/// move at once; large files take 2 permits (see below) so at most 4 big
/// transfers run concurrently.
const TRANSFER_CONCURRENCY: usize = 8;

/// Files larger than this take 2 transfer permits instead of 1, so big
/// transfers don't saturate a slow link and time each other out.
const LARGE_FILE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Copy)]
enum TransferKind {
    Upload,
    Download,
    /// Diverging file: preserve the local copy, then download the server's.
    ConflictDownload,
}

/// A file transfer queued for concurrent execution after the mkdir pass.
struct TransferItem {
    key: String,
    remote_full: String,
    local_full: PathBuf,
    kind: TransferKind,
    /// Expected size (remote for downloads, local for uploads).
    size: u64,
    /// Local mtime for uploads; recomputed after downloads.
    mtime: i64,
    /// Remote etag for downloads; obtained from the PUT for uploads.
    etag: Option<String>,
}

/// Transient errors (network blips, 5xx) are worth retrying; a 4xx is not.
fn is_transient(e: &AppError) -> bool {
    match e {
        AppError::Http(_) | AppError::Io(_) => true,
        AppError::Server { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Max attempts per transfer before giving up.
const TRANSFER_ATTEMPTS: u32 = 3;

/// The mass-deletion guard only engages from this many planned deletions on
/// (AND at least half of that side's entries), so ordinary folder deletions
/// still propagate while a vanished tree cannot wipe the other side.
const DELETION_GUARD_MIN: usize = 10;

/// Should a planned deletion sweep be refused as probable state loss?
/// True when it would remove at least [`DELETION_GUARD_MIN`] entries AND at
/// least half of that side. Boundaries are pinned by unit tests.
fn deletion_sweep_suspicious(planned: usize, side_total: usize) -> bool {
    planned >= DELETION_GUARD_MIN && planned * 2 >= side_total
}

/// Execute one transfer with retries, reporting progress. Emits `file_done` on
/// success or `file_aborted` on final failure so the UI never shows a stuck
/// in-flight file.
async fn run_transfer(
    client: &WebDavClient,
    item: &TransferItem,
    reporter: &Reporter,
) -> AppResult<JournalEntry> {
    let dir = match item.kind {
        TransferKind::Upload => Direction::Upload,
        _ => Direction::Download,
    };

    let mut last_err = None;
    for attempt in 1..=TRANSFER_ATTEMPTS {
        reporter.file_start(&item.key, dir, item.size);
        match transfer_once(client, item, reporter).await {
            Ok(entry) => {
                reporter.file_done(&item.key, dir, entry.size);
                return Ok(entry);
            }
            Err(e) => {
                let transient = is_transient(&e);
                log::warn!(
                    "transfer {} attempt {attempt}/{TRANSFER_ATTEMPTS} failed: {e}",
                    item.key
                );
                last_err = Some(e);
                if !transient || attempt == TRANSFER_ATTEMPTS {
                    break;
                }
                // Reset the in-flight row and back off before retrying.
                reporter.file_aborted(&item.key);
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
            }
        }
    }
    reporter.file_aborted(&item.key);
    // Gave up after all retries: drop the partial download temp so a later run
    // starts clean (avoids resuming against a possibly-changed remote file).
    if matches!(item.kind, TransferKind::Download | TransferKind::ConflictDownload) {
        let _ = tokio::fs::remove_file(tmp_path(&item.local_full)).await;
    }
    Err(last_err.unwrap_or_else(|| AppError::msg("transfer failed")))
}

/// A single transfer attempt (no retry, no start/done events).
async fn transfer_once(
    client: &WebDavClient,
    item: &TransferItem,
    reporter: &Reporter,
) -> AppResult<JournalEntry> {
    let pr = reporter.clone();
    let pk = item.key.clone();
    let total = item.size;
    match item.kind {
        TransferKind::Upload => {
            // A fresh progress reporter for each upload attempt (the closure is
            // consumed by the call, and a 413 fallback needs a second one).
            let progress = |reporter: &Reporter, key: &str| {
                let pr = reporter.clone();
                let pk = key.to_string();
                move |sent: u64| pr.file_progress(&pk, sent.min(total), total)
            };
            // Chunked upload assembles atomically onto the real path via its own
            // session + final MOVE, so no separate temp+move is needed.
            let chunked = |client: &WebDavClient, prog| {
                let client = client.clone();
                let remote = item.remote_full.clone();
                let local = item.local_full.clone();
                async move { client.put_file_chunked(&remote, &local, prog).await }
            };

            let etag = if item.size >= crate::webdav::CHUNK_UPLOAD_THRESHOLD {
                // Big file: skip the doomed whole-file PUT, chunk straight away.
                match chunked(client, progress(reporter, &item.key)).await? {
                    Some(e) => Some(e),
                    None => client.stat(&item.remote_full).await?.and_then(|e| e.etag),
                }
            } else {
                // Fast path — a single streamed PUT to a remote temp, then an
                // atomic MOVE onto the real path. An interrupted/partial upload
                // thus never touches the real file (otherwise the next sync sees
                // the remote "changed" — a truncated partial — and downloads it
                // back instead of finishing, spawning conflicts).
                let remote_tmp = format!("{}{}", item.remote_full, TMP_SUFFIX);
                match client
                    .put_file_streaming(&remote_tmp, &item.local_full, progress(reporter, &item.key))
                    .await
                {
                    Ok(_) => {
                        client.move_replace(&remote_tmp, &item.remote_full).await?;
                        client.stat(&item.remote_full).await?.and_then(|e| e.etag)
                    }
                    // Server refused the whole-file PUT as too large → transparently
                    // fall back to chunked upload for this file.
                    Err(AppError::Server { status: 413, .. }) => {
                        let _ = client.delete(&remote_tmp).await;
                        log::info!("upload {} exceeded server PUT limit — using chunked upload", item.key);
                        match chunked(client, progress(reporter, &item.key)).await? {
                            Some(e) => Some(e),
                            None => client.stat(&item.remote_full).await?.and_then(|e| e.etag),
                        }
                    }
                    Err(e) => {
                        let _ = client.delete(&remote_tmp).await; // clean the partial temp
                        return Err(e);
                    }
                }
            };
            Ok(JournalEntry { is_dir: false, etag, size: item.size, local_mtime: item.mtime })
        }
        TransferKind::Download | TransferKind::ConflictDownload => {
            // Download to a temp file first. A failed/partial transfer must
            // never touch the real path — a truncated local file would look
            // "changed" next sync and manufacture an endless conflict loop.
            let tmp = tmp_path(&item.local_full);
            let dl = client
                .download_to_file(&item.remote_full, &tmp, move |done, t| {
                    pr.file_progress(&pk, done.min(total), t.unwrap_or(total));
                })
                .await;
            if let Err(e) = dl {
                // Keep the partial temp so the retry resumes from it.
                return Err(e);
            }
            // Safety gate: only publish a download whose size matches what the
            // server reported. A short/truncated temp must NEVER overwrite the
            // local file — bail (keeping the temp for a resume) instead.
            let got = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            if item.size > 0 && got != item.size {
                return Err(AppError::msg(format!(
                    "incomplete download ({got}/{} bytes) — not overwriting local",
                    item.size
                )));
            }
            // Verified complete. For a genuine conflict, preserve the local copy first.
            if matches!(item.kind, TransferKind::ConflictDownload) {
                reporter.conflict(&item.key);
                let conflicted = conflicted_name(&item.local_full);
                let _ = tokio::fs::rename(&item.local_full, &conflicted).await;
            }
            tokio::fs::rename(&tmp, &item.local_full).await?;
            let size = std::fs::metadata(&item.local_full).map(|m| m.len()).unwrap_or(item.size);
            let mtime = local_mtime(&item.local_full);
            Ok(JournalEntry { is_dir: false, etag: item.etag.clone(), size, local_mtime: mtime })
        }
    }
}

/// Suffix for in-progress download temp files. Skipped by the local walk so
/// they are never mistaken for real content to upload.
const TMP_SUFFIX: &str = ".ncsync-tmp";

fn tmp_path(target: &Path) -> PathBuf {
    let mut name =
        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.push_str(TMP_SUFFIX);
    target.with_file_name(name)
}

/// The prepared state of one folder pair: both trees walked, journal loaded
/// and the transfer plan tallied — ready to execute. Preparing all folders
/// before executing any lets the UI show stable whole-run totals.
pub struct Prepared {
    remote: HashMap<String, RemoteMeta>,
    local: HashMap<String, LocalMeta>,
    journal: Journal,
    keys: BTreeSet<String>,
    pub files_total: u64,
    pub bytes_total: u64,
    /// Same-size both-sides files headed for the verification pass. Counted
    /// separately from the transfer plan: they are *compared* (and usually
    /// adopted in place), not downloaded — showing them as pending download
    /// volume made a first sync of pre-existing data look like it was
    /// re-fetching everything.
    pub verify_files: u64,
}

/// Walk both sides of a folder pair and tally what a sync would transfer.
/// Reports scan progress so the UI can show that something is happening.
pub async fn prepare(
    journal_dir: &Path,
    client: &WebDavClient,
    folder: &SyncFolder,
    ignore: &[String],
    reporter: &Reporter,
    cancel: &Cancel,
) -> AppResult<Prepared> {
    let local_root = PathBuf::from(&folder.local_path);
    tokio::fs::create_dir_all(&local_root).await?;
    ensure_remote_dir(client, &folder.remote_path).await?;

    let journal = Journal::load(journal_dir, &folder.id)?;
    let remote = walk_remote(client, &folder.remote_path, cancel, |found| {
        reporter.scan_progress(&folder.remote_path, found);
    })
    .await?;
    let local = walk_local(&local_root)?;

    // All paths seen anywhere, sorted so parents precede children.
    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(remote.keys().cloned());
    keys.extend(local.keys().cloned());
    keys.extend(journal.entries.keys().cloned());

    let (mut files_total, mut bytes_total, mut verify_files) = (0u64, 0u64, 0u64);
    for key in &keys {
        if is_ignored(key, ignore) {
            continue;
        }
        let (r, l) = (remote.get(key), local.get(key));
        match classify(r, l, journal.entries.get(key)) {
            Decision::Download => {
                files_total += 1;
                bytes_total += r.map_or(0, |r| r.size);
            }
            Decision::Conflict => {
                // Same-size pairs go to the verification pass (compared, not
                // transferred) — count them as such, not as download volume.
                if matches!((r, l), (Some(r), Some(l)) if !r.is_dir && r.size == l.size) {
                    verify_files += 1;
                } else {
                    files_total += 1;
                    bytes_total += r.map_or(0, |r| r.size);
                }
            }
            Decision::Upload => {
                files_total += 1;
                bytes_total += l.map_or(0, |l| l.size);
            }
            _ => {}
        }
    }

    Ok(Prepared { remote, local, journal, keys, files_total, bytes_total, verify_files })
}

/// Convenience wrapper: prepare a single folder, report its plan and sync it.
/// Production uses the two-phase prepare/sync_prepared flow; this stays for
/// the live integration tests.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn sync_folder(
    journal_dir: &Path,
    client: &WebDavClient,
    folder: &SyncFolder,
    reporter: &Reporter,
    ignore: &[String],
) -> AppResult<SyncStats> {
    let cancel = Cancel::never();
    let prepared = prepare(journal_dir, client, folder, ignore, reporter, &cancel).await?;
    reporter.session_plan(prepared.files_total, prepared.bytes_total, prepared.verify_files);
    sync_prepared(journal_dir, client, folder, prepared, reporter, ignore, &cancel).await
}

/// Execute a previously [`prepare`]d sync. Returns per-run statistics.
///
/// Cancellation (pause / folder disable) is cooperative: planning stops, no
/// new transfers start, in-flight ones are aborted (download temps are kept
/// for resume), and the journal is saved with every untouched path carried
/// over — so a paused run is indistinguishable from one that never got there.
pub async fn sync_prepared(
    journal_dir: &Path,
    client: &WebDavClient,
    folder: &SyncFolder,
    prepared: Prepared,
    reporter: &Reporter,
    ignore: &[String],
    cancel: &Cancel,
) -> AppResult<SyncStats> {
    let local_root = PathBuf::from(&folder.local_path);
    let Prepared { remote, local, journal, keys, .. } = prepared;

    let mut new_journal = Journal::default();
    let mut stats = SyncStats::default();

    // Deletions are collected and applied child-first afterwards.
    let mut delete_local: Vec<String> = Vec::new();
    let mut delete_remote: Vec<String> = Vec::new();
    let mut verify_queue: Vec<VerifyItem> = Vec::new();
    let mut transfer_queue: Vec<TransferItem> = Vec::new();
    let mut since_save = 0usize;

    for key in &keys {
        if cancel.is_cancelled() {
            break;
        }
        if is_ignored(key, ignore) {
            continue;
        }
        let r = remote.get(key);
        let l = local.get(key);
        let j = journal.entries.get(key);
        let remote_full = remote_join(&folder.remote_path, key);
        let local_full = local_root.join(key);

        let result: AppResult<()> = async {
            match classify(r, l, j) {
                Decision::None => {
                    if let (Some(r), Some(l)) = (r, l) {
                        let size = if r.is_dir { 0 } else { l.size };
                        record(&mut new_journal, key, r.is_dir, r.etag.clone(), size, l.mtime);
                    }
                }
                // Transfers are queued and run concurrently after all
                // directories have been created (see below).
                Decision::Download => {
                    let r = r.unwrap();
                    transfer_queue.push(TransferItem {
                        key: key.clone(),
                        remote_full: remote_full.clone(),
                        local_full: local_full.clone(),
                        kind: TransferKind::Download,
                        size: r.size,
                        mtime: 0,
                        etag: r.etag.clone(),
                    });
                }
                Decision::Upload => {
                    let l = l.unwrap();
                    transfer_queue.push(TransferItem {
                        key: key.clone(),
                        remote_full: remote_full.clone(),
                        local_full: local_full.clone(),
                        kind: TransferKind::Upload,
                        size: l.size,
                        mtime: l.mtime,
                        etag: None,
                    });
                }
                Decision::Conflict => {
                    let r = r.unwrap();
                    let l = l.unwrap();
                    // Same-size files that "changed" on both sides are usually
                    // identical content (first sync of pre-existing data).
                    // Queue for concurrent verification; genuinely diverging
                    // ones fall through to a conflict download.
                    if !r.is_dir && r.size == l.size {
                        verify_queue.push(VerifyItem {
                            key: key.clone(),
                            remote_full: remote_full.clone(),
                            local_full: local_full.clone(),
                            size: l.size,
                            mtime: l.mtime,
                            etag: r.etag.clone(),
                            checksums: r.checksums.clone(),
                        });
                    } else {
                        transfer_queue.push(TransferItem {
                            key: key.clone(),
                            remote_full: remote_full.clone(),
                            local_full: local_full.clone(),
                            kind: TransferKind::ConflictDownload,
                            size: r.size,
                            mtime: 0,
                            etag: r.etag.clone(),
                        });
                    }
                }
                Decision::MkdirLocal => {
                    let r = r.unwrap();
                    tokio::fs::create_dir_all(&local_full).await?;
                    let mtime = local_mtime(&local_full);
                    record(&mut new_journal, key, true, r.etag.clone(), 0, mtime);
                }
                Decision::MkdirRemote => {
                    let l = l.unwrap();
                    client.mkcol(&remote_full).await?;
                    let etag = client.stat(&remote_full).await?.and_then(|e| e.etag);
                    record(&mut new_journal, key, true, etag, 0, l.mtime);
                }
                Decision::DeleteRemote => delete_remote.push(key.clone()),
                Decision::DeleteLocal => delete_local.push(key.clone()),
            }
            Ok(())
        }
        .await;

        if let Err(e) = result {
            // A single failing file shouldn't abort the whole folder.
            reporter.error(key, &e.to_string());
            log::warn!("sync {key}: {e}");
        }

        // Persist progress periodically so an interrupted run (crash, app
        // restart) resumes where it left off instead of starting over —
        // crucial for the potentially long first-sync verification pass.
        since_save += 1;
        if since_save >= 100 {
            since_save = 0;
            if let Err(e) = new_journal.save(journal_dir, &folder.id) {
                log::warn!("incremental journal save failed: {e}");
            }
        }
    }

    // Run queued uploads/downloads concurrently — network transfers are
    // I/O-bound, so several in flight hide each other's round-trip latency
    // (the gap you'd otherwise see between files). All directories were
    // created in the pass above, so no per-file MKCOL is needed. Results are
    // recorded serially as they complete.
    if !transfer_queue.is_empty() {
        use futures_util::stream::{self, StreamExt};

        // Weighted concurrency: a shared permit budget of TRANSFER_CONCURRENCY,
        // where large files take 2 permits and small ones 1. So up to 4 small
        // files transfer at once, but only 2 large ones — big videos over a
        // flaky/slow link don't saturate it and time each other out.
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(TRANSFER_CONCURRENCY));
        let mut stream = stream::iter(transfer_queue.into_iter().map(|item| {
            let client = client.clone();
            let reporter = reporter.clone();
            let sem = sem.clone();
            let cancel = cancel.clone();
            async move {
                let weight = if item.size > LARGE_FILE_BYTES { 2 } else { 1 };
                let _permit = sem.acquire_many(weight).await.ok();
                // Queued but not yet started when the pause hit — skip quietly.
                if cancel.is_cancelled() {
                    return (item, None);
                }
                // Race the transfer against cancellation so a pause aborts even
                // a large in-flight file. An aborted download keeps its temp
                // (resumed by the next run); an aborted upload only ever
                // touched a remote `.ncsync-tmp`, which uploads overwrite.
                tokio::select! {
                    outcome = run_transfer(&client, &item, &reporter) => (item, Some(outcome)),
                    _ = cancel.cancelled() => {
                        reporter.file_aborted(&item.key);
                        (item, None)
                    }
                }
            }
        }))
        // Poll more futures than the budget so freed permits are picked up
        // immediately; the semaphore is the real limiter.
        .buffer_unordered(TRANSFER_CONCURRENCY * 2);

        while let Some((item, outcome)) = stream.next().await {
            match outcome {
                // Cancelled before/while transferring: recorded by the final
                // carry-over pass so the next run re-plans this file.
                None => {}
                Some(Ok(entry)) => {
                    match item.kind {
                        TransferKind::Upload => stats.uploaded += 1,
                        TransferKind::Download => stats.downloaded += 1,
                        TransferKind::ConflictDownload => stats.conflicts += 1,
                    }
                    new_journal.entries.insert(item.key.clone(), entry);
                }
                Some(Err(e)) => {
                    reporter.error(&item.key, &e.to_string());
                    log::warn!("transfer {}: {e}", item.key);
                }
            }
            since_save += 1;
            if since_save >= 100 {
                since_save = 0;
                if let Err(e) = new_journal.save(journal_dir, &folder.id) {
                    log::warn!("incremental journal save failed: {e}");
                }
            }
        }
    }

    // Verify queued same-size files concurrently: hash locally when the
    // server stored a checksum (no download), else stream-compare.
    if !verify_queue.is_empty() {
        use futures_util::stream::{self, StreamExt};

        // Results are handled as they arrive — each verified file is
        // reported and journaled immediately, so the UI shows live progress
        // and an interrupted run resumes from wherever it got to.
        let mut results = stream::iter(verify_queue.into_iter().map(|item| {
            let client = client.clone();
            let cancel = cancel.clone();
            async move {
                if cancel.is_cancelled() {
                    return (item, None);
                }
                let identical = match checksum_matches(&item.local_full, &item.checksums).await
                {
                    Some(matched) => Ok(matched),
                    None => {
                        client
                            .compare_with_local(&item.remote_full, &item.local_full, |_| {})
                            .await
                    }
                };
                (item, Some(identical))
            }
        }))
        .buffer_unordered(VERIFY_CONCURRENCY);

        while let Some((item, outcome)) = results.next().await {
            match outcome {
                // Cancelled before verification: the final carry-over pass
                // keeps the old journal entry for the next run.
                None => {}
                Some(Ok(true)) => {
                    record(
                        &mut new_journal,
                        &item.key,
                        false,
                        item.etag.clone(),
                        item.size,
                        item.mtime,
                    );
                    reporter.verify_done(&item.key, item.size);
                }
                Some(Ok(false)) => {
                    // Genuinely diverging content — download remote to a temp
                    // file and only preserve+swap the local copy on success, so
                    // a failed download never orphans the original or spawns a
                    // duplicate conflicted-copy on the next run.
                    let tmp = tmp_path(&item.local_full);
                    match client.download_to_file(&item.remote_full, &tmp, |_, _| {}).await {
                        Ok(()) => {
                            reporter.conflict(&item.key);
                            let conflicted = conflicted_name(&item.local_full);
                            let _ = tokio::fs::rename(&item.local_full, &conflicted).await;
                            if let Err(e) = tokio::fs::rename(&tmp, &item.local_full).await {
                                let _ = tokio::fs::remove_file(&tmp).await;
                                reporter.error(&item.key, &e.to_string());
                            } else {
                                let size = std::fs::metadata(&item.local_full)
                                    .map(|m| m.len())
                                    .unwrap_or(item.size);
                                let mtime = local_mtime(&item.local_full);
                                record(
                                    &mut new_journal,
                                    &item.key,
                                    false,
                                    item.etag.clone(),
                                    size,
                                    mtime,
                                );
                                stats.conflicts += 1;
                            }
                        }
                        Err(e) => {
                            let _ = tokio::fs::remove_file(&tmp).await;
                            reporter.error(&item.key, &e.to_string());
                            log::warn!("conflict download {}: {e}", item.key);
                        }
                    }
                }
                Some(Err(e)) => {
                    reporter.error(&item.key, &e.to_string());
                    log::warn!("verify {}: {e}", item.key);
                }
            }
            since_save += 1;
            if since_save >= 100 {
                since_save = 0;
                if let Err(e) = new_journal.save(journal_dir, &folder.id) {
                    log::warn!("incremental journal save failed: {e}");
                }
            }
        }
    }

    // ── Mass-deletion guard ────────────────────────────────────────────────
    // A sweep that would delete most of one side is state loss (an unmounted
    // disk, a renamed or emptied local folder, a server answering with a
    // hollow tree) — not intent. Refuse it and drop the affected journal
    // entries: the next run then sees the surviving files as unknown and
    // RESTORES them on the missing side instead. When in doubt this engine
    // may re-transfer files; it must never mass-delete them.
    if deletion_sweep_suspicious(delete_remote.len(), remote.len()) {
        log::warn!(
            "{}: refusing to delete {}/{} remote entries — local side looks lost",
            folder.remote_path,
            delete_remote.len(),
            remote.len()
        );
        reporter.error(
            &folder.remote_path,
            "mass deletion refused: the local folder looks missing or emptied — server files kept, they will be restored locally",
        );
        stats.blocked_deletions += delete_remote.len() as u32;
        delete_remote.clear();
    }
    if deletion_sweep_suspicious(delete_local.len(), local.len()) {
        log::warn!(
            "{}: refusing to delete {}/{} local entries — remote side looks lost",
            folder.remote_path,
            delete_local.len(),
            local.len()
        );
        reporter.error(
            &folder.remote_path,
            "mass deletion refused: the server folder looks missing or emptied — local files kept, they will be restored remotely",
        );
        stats.blocked_deletions += delete_local.len() as u32;
        delete_local.clear();
    }

    // Apply deletions child-first (reverse sorted order), so a folder's own
    // entries are gone before we reach the folder itself.
    for key in delete_remote.into_iter().rev() {
        // A deletion skipped by a pause keeps its journal entry (final
        // carry-over) — without it the next run would see an unknown remote
        // file and download it back instead of finishing the delete.
        if cancel.is_cancelled() {
            continue;
        }
        let full = remote_join(&folder.remote_path, &key);
        // A directory delete is recursive on the server, so only issue it once
        // the folder is actually empty — anything still inside was added
        // remotely and must be kept, not swept away with the folder.
        if remote.get(&key).map_or(false, |r| r.is_dir) && !remote_dir_empty(client, &full).await {
            continue;
        }
        if client.delete(&full).await.is_ok() {
            reporter.deleted(&key, true);
            stats.deleted_remote += 1;
        }
    }
    for key in delete_local.into_iter().rev() {
        if cancel.is_cancelled() {
            continue;
        }
        let path = local_root.join(&key);
        // Non-recursive for directories: it fails (and the folder is kept) if a
        // locally-added file still sits inside, mirroring the remote guard.
        let res = if path.is_dir() {
            tokio::fs::remove_dir(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        if res.is_ok() {
            reporter.deleted(&key, false);
            stats.deleted_local += 1;
        }
    }

    // A cancelled run may not have reached every planned path. Carry the old
    // journal entry for anything the run didn't get to (fresh results above
    // always win), so untouched divergences — pending deletions included —
    // are re-planned identically next run instead of being misread as new.
    if cancel.is_cancelled() {
        for (key, entry) in &journal.entries {
            new_journal
                .entries
                .entry(key.clone())
                .or_insert_with(|| entry.clone());
        }
    }

    new_journal.save(journal_dir, &folder.id)?;
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// How many directory listings run in parallel during the remote walk.
const WALK_CONCURRENCY: usize = 6;

async fn walk_remote(
    client: &WebDavClient,
    base: &str,
    cancel: &Cancel,
    mut on_progress: impl FnMut(u64),
) -> AppResult<HashMap<String, RemoteMeta>> {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let base_norm = format!("/{}", base.trim_matches('/'));
    let mut out = HashMap::new();
    let mut pending: Vec<String> = vec![base_norm.clone()];
    let mut inflight = FuturesUnordered::new();
    let mut entries_seen = 0u64;

    let spawn = |dir: String| {
        let client = client.clone();
        async move { client.list(&dir).await }
    };

    while !pending.is_empty() || !inflight.is_empty() {
        // A partial tree must never be mistaken for a complete one (missing
        // remote entries would classify as local-only additions), so a pause
        // aborts the walk with an error the runner recognizes as cancellation.
        if cancel.is_cancelled() {
            return Err(AppError::msg("sync cancelled"));
        }
        while inflight.len() < WALK_CONCURRENCY {
            match pending.pop() {
                Some(dir) => inflight.push(spawn(dir)),
                None => break,
            }
        }
        let Some(result) = inflight.next().await else {
            break;
        };
        let entries = match result {
            Ok(e) => e,
            Err(AppError::Server { status: 404, .. }) => continue,
            Err(e) => return Err(e),
        };
        for e in entries {
            let rel = rel_to_base(&base_norm, &e.path);
            // Skip our own in-progress upload temps (orphans from a failed
            // upload) so they're never treated as real remote files.
            if rel.is_empty() || rel.ends_with(TMP_SUFFIX) {
                continue;
            }
            entries_seen += 1;
            if e.is_dir {
                pending.push(e.path.clone());
                out.insert(
                    rel,
                    RemoteMeta { is_dir: true, size: 0, etag: e.etag, checksums: None },
                );
            } else {
                out.insert(
                    rel,
                    RemoteMeta {
                        is_dir: false,
                        size: e.size,
                        etag: e.etag,
                        checksums: e.checksums,
                    },
                );
            }
        }
        on_progress(entries_seen);
    }
    Ok(out)
}

/// Walk the local tree. Errors are propagated, not swallowed: an unreadable
/// directory silently skipped here would downstream read as "the user deleted
/// all of this" and propagate a deletion sweep to the server. The only
/// tolerated failure is a path vanishing mid-walk (equivalent to it being
/// deleted a moment later).
fn walk_local(root: &Path) -> AppResult<HashMap<String, LocalMeta>> {
    fn vanished(e: &walkdir::Error) -> bool {
        e.io_error().map_or(false, |io| io.kind() == std::io::ErrorKind::NotFound)
    }

    // Probe the root explicitly — WalkDir would yield a single error entry,
    // but the message ("cannot read local folder") matters more than that.
    std::fs::read_dir(root)
        .map_err(|e| AppError::msg(format!("cannot read local folder {}: {e}", root.display())))?;

    let mut out = HashMap::new();
    for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if vanished(&e) => continue,
            Err(e) => return Err(AppError::msg(format!("local scan failed: {e}"))),
        };
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if rel.is_empty() || is_conflicted(&rel) || rel.ends_with(TMP_SUFFIX) {
            continue;
        }
        let md = match entry.metadata() {
            Ok(md) => md,
            Err(e) if vanished(&e) => continue,
            Err(e) => return Err(AppError::msg(format!("local scan failed for {rel}: {e}"))),
        };
        let is_dir = md.is_dir();
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(
            rel,
            LocalMeta {
                is_dir,
                size: if is_dir { 0 } else { md.len() },
                mtime,
            },
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn record(journal: &mut Journal, key: &str, is_dir: bool, etag: Option<String>, size: u64, mtime: i64) {
    journal.entries.insert(
        key.to_string(),
        JournalEntry { is_dir, etag, size, local_mtime: mtime },
    );
}

fn local_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pick a remote path for a NEW folder pair that cannot absorb unrelated
/// pre-existing server data: the desired path if it is free or an empty
/// folder, else `"<path> 2"`, `"<path> 3"`, … A local folder paired with an
/// already-populated same-name server folder must become a second version
/// beside it — never merge into (or later delete from) data it didn't create.
/// Pulling an existing server folder down deliberately bypasses this.
pub async fn unique_remote_path(client: &WebDavClient, desired: &str) -> AppResult<String> {
    let base = format!("/{}", desired.trim_matches('/'));
    if base == "/" {
        return Ok(base); // pairing the account root is always explicit
    }
    for n in 1..100u32 {
        let candidate = if n == 1 { base.clone() } else { format!("{base} {n}") };
        match client.stat(&candidate).await? {
            None => return Ok(candidate),
            Some(entry) if entry.is_dir => {
                // An empty folder holds nothing to absorb — safe to use.
                if client.list(&candidate).await?.is_empty() {
                    return Ok(candidate);
                }
            }
            Some(_) => {} // a file by that name: keep probing suffixes
        }
    }
    Err(AppError::msg(format!("no free remote folder name found for {base}")))
}

/// Create a remote collection and all of its ancestors (idempotent).
async fn ensure_remote_dir(client: &WebDavClient, path: &str) -> AppResult<()> {
    let mut acc = String::new();
    for seg in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(seg);
        client.mkcol(&acc).await?;
    }
    Ok(())
}

pub(super) fn remote_join(base: &str, rel: &str) -> String {
    let b = base.trim_matches('/');
    if b.is_empty() {
        format!("/{}", rel)
    } else {
        format!("/{}/{}", b, rel)
    }
}

fn rel_to_base(base: &str, path: &str) -> String {
    let p = path.trim_end_matches('/');
    let b = base.trim_end_matches('/');
    let stripped = if b.is_empty() {
        p.trim_start_matches('/')
    } else {
        p.strip_prefix(b).unwrap_or(p).trim_start_matches('/')
    };
    stripped.to_string()
}

/// Compare a local file against a Nextcloud `oc:checksums` string
/// (e.g. `"SHA1:ab12… MD5:cd34…"`). Returns `None` when no supported
/// algorithm is present, so the caller can fall back to a byte comparison.
async fn checksum_matches(local: &Path, checksums: &Option<String>) -> Option<bool> {
    let sums = checksums.as_deref()?;
    for token in sums.split_whitespace() {
        let (algo, expected) = token.split_once(':')?;
        let actual = match algo.to_ascii_uppercase().as_str() {
            "SHA1" => crate::webdav::hash_file_sha1(local).await?,
            "MD5" => crate::webdav::hash_file_md5(local).await?,
            _ => continue,
        };
        return Some(actual.eq_ignore_ascii_case(expected));
    }
    None
}

const CONFLICT_MARK: &str = " (conflicted copy";

fn is_conflicted(rel: &str) -> bool {
    rel.contains(CONFLICT_MARK)
}

/// If `name` is a conflicted-copy filename, return the original name.
pub(super) fn conflict_original(name: &str) -> Option<String> {
    let start = name.find(CONFLICT_MARK)?;
    let close_rel = name[start..].find(')')?;
    let close = start + close_rel;
    Some(format!("{}{}", &name[..start], &name[close + 1..]))
}

/// Does `rel` (a path relative to the folder root) match any ignore pattern?
/// Patterns without `/` match any path segment (so `node_modules` also ignores
/// its children); patterns with `/` match the whole relative path.
fn is_ignored(rel: &str, patterns: &[String]) -> bool {
    for pat in patterns {
        let pat = pat.trim();
        if pat.is_empty() {
            continue;
        }
        if pat.contains('/') {
            if wildcard_match(pat.as_bytes(), rel.as_bytes()) {
                return true;
            }
        } else if rel.split('/').any(|seg| wildcard_match(pat.as_bytes(), seg.as_bytes())) {
            return true;
        }
    }
    false
}

/// Classic glob matcher supporting `*` and `?`, with linear backtracking.
fn wildcard_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut c) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while c < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[c]) {
            p += 1;
            c += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = p;
            mark = c;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            c = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

fn conflicted_name(path: &Path) -> PathBuf {
    let date = chrono::Utc::now().format("%Y-%m-%d %H%M%S");
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let name = format!("{stem}{CONFLICT_MARK} {date}){ext}");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rdir(etag: &str) -> RemoteMeta {
        RemoteMeta { is_dir: true, size: 0, etag: Some(etag.into()), checksums: None }
    }
    fn ldir() -> LocalMeta {
        LocalMeta { is_dir: true, size: 0, mtime: 0 }
    }
    fn jdir(etag: &str) -> JournalEntry {
        JournalEntry { is_dir: true, etag: Some(etag.into()), size: 0, local_mtime: 0 }
    }

    // Regression: a folder that ever held files gets a different remote ETag
    // than the (empty-folder) ETag recorded in the journal. Deleting it locally
    // must still delete it remotely — not re-create it — even with that mismatch.
    #[test]
    fn deleting_local_dir_with_stale_etag_removes_remote() {
        // remote etag "E1" (has children) != journal etag "E0" (recorded empty)
        let d = classify(Some(&rdir("E1")), None, Some(&jdir("E0")));
        assert_eq!(d, Decision::DeleteRemote, "known folder gone locally -> delete remote");
    }

    #[test]
    fn deleting_remote_dir_with_stale_state_removes_local() {
        let d = classify(None, Some(&ldir()), Some(&jdir("E0")));
        assert_eq!(d, Decision::DeleteLocal, "known folder gone remotely -> delete local");
    }

    #[test]
    fn unknown_remote_dir_is_mirrored_not_deleted() {
        // No journal entry -> the folder is new remotely, not a local deletion.
        assert_eq!(classify(Some(&rdir("E1")), None, None), Decision::MkdirLocal);
    }

    #[test]
    fn unknown_local_dir_is_mirrored_not_deleted() {
        assert_eq!(classify(None, Some(&ldir()), None), Decision::MkdirRemote);
    }

    #[test]
    fn two_present_dirs_are_left_alone() {
        assert_eq!(classify(Some(&rdir("E1")), Some(&ldir()), Some(&jdir("E0"))), Decision::None);
    }

    fn rfile(etag: &str, size: u64) -> RemoteMeta {
        RemoteMeta { is_dir: false, size, etag: Some(etag.into()), checksums: None }
    }
    fn lfile(size: u64, mtime: i64) -> LocalMeta {
        LocalMeta { is_dir: false, size, mtime }
    }
    fn jfile(etag: &str, size: u64, mtime: i64) -> JournalEntry {
        JournalEntry { is_dir: false, etag: Some(etag.into()), size, local_mtime: mtime }
    }

    /// The full reconciliation matrix for a file, spelled out. The invariants
    /// that keep data safe: nothing is EVER deleted without a journal entry
    /// proving the surviving side is unchanged since the last sync, and an
    /// unknown pair (no journal) always merges — never deletes.
    #[test]
    fn classify_file_matrix() {
        // Fresh pair (no journal): both sides merge.
        assert_eq!(classify(Some(&rfile("E", 1)), None, None), Decision::Download,
            "unknown remote file downloads");
        assert_eq!(classify(None, Some(&lfile(1, 10)), None), Decision::Upload,
            "unknown local file uploads");
        assert_eq!(classify(Some(&rfile("E", 1)), Some(&lfile(1, 10)), None), Decision::Conflict,
            "unknown on both sides goes through conflict/verify (identical content is adopted)");

        // Journaled and unchanged: nothing to do.
        assert_eq!(
            classify(Some(&rfile("E", 1)), Some(&lfile(1, 10)), Some(&jfile("E", 1, 10))),
            Decision::None
        );

        // Exactly one side changed.
        assert_eq!(
            classify(Some(&rfile("E2", 1)), Some(&lfile(1, 10)), Some(&jfile("E", 1, 10))),
            Decision::Download,
            "remote edit downloads"
        );
        assert_eq!(
            classify(Some(&rfile("E", 1)), Some(&lfile(2, 11)), Some(&jfile("E", 1, 10))),
            Decision::Upload,
            "local edit uploads"
        );
        assert_eq!(
            classify(Some(&rfile("E2", 1)), Some(&lfile(2, 11)), Some(&jfile("E", 1, 10))),
            Decision::Conflict,
            "both edited -> conflict"
        );

        // Deletions require an unchanged counterpart; an edit always outranks
        // a deletion (the survivor is restored, not removed).
        assert_eq!(
            classify(None, Some(&lfile(1, 10)), Some(&jfile("E", 1, 10))),
            Decision::DeleteLocal,
            "remote deleted, local unchanged -> delete local"
        );
        assert_eq!(
            classify(None, Some(&lfile(2, 11)), Some(&jfile("E", 1, 10))),
            Decision::Upload,
            "remote deleted but local edited -> local edit wins, file restored"
        );
        assert_eq!(
            classify(Some(&rfile("E", 1)), None, Some(&jfile("E", 1, 10))),
            Decision::DeleteRemote,
            "local deleted, remote unchanged -> delete remote"
        );
        assert_eq!(
            classify(Some(&rfile("E2", 1)), None, Some(&jfile("E", 1, 10))),
            Decision::Download,
            "local deleted but remote edited -> remote edit wins, file restored"
        );

        // Gone on both sides: just forget it.
        assert_eq!(classify(None, None, Some(&jfile("E", 1, 10))), Decision::None);

        // A file/directory type flip is left alone rather than guessed at.
        assert_eq!(classify(Some(&rdir("E")), Some(&lfile(1, 10)), None), Decision::None);
    }

    // The mass-deletion guard boundaries: ordinary deletions (below the
    // minimum, or a small share of a big tree) propagate; a sweep of at least
    // DELETION_GUARD_MIN entries covering >= half of one side is refused.
    #[test]
    fn deletion_guard_boundaries() {
        assert!(!deletion_sweep_suspicious(0, 0), "empty plan is never suspicious");
        assert!(!deletion_sweep_suspicious(9, 9), "below the minimum always propagates (even 100%)");
        assert!(deletion_sweep_suspicious(10, 10), "wholesale wipe at the minimum is refused");
        assert!(deletion_sweep_suspicious(10, 20), "exactly half is refused");
        assert!(!deletion_sweep_suspicious(10, 21), "just under half propagates");
        assert!(!deletion_sweep_suspicious(50, 1000), "a small share of a big tree propagates");
        assert!(deletion_sweep_suspicious(600, 1000), "most of a big tree is refused");
    }

    // An unreadable directory must abort the local walk with an error — a
    // silently-skipped subtree would downstream classify as "the user deleted
    // all of this" and propagate a deletion sweep.
    #[test]
    fn walk_local_errors_on_unreadable_subdir() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = std::env::temp_dir().join(format!("cirwalk_{}", std::process::id()));
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(root.join("ok.txt"), b"x").unwrap();
        std::fs::write(locked.join("hidden.txt"), b"y").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = walk_local(&root);

        // Restore permissions before asserting so cleanup always succeeds.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        if matches!(std::fs::metadata("/proc/self").map(|m| m.uid()), Ok(0)) {
            return; // root ignores permission bits — the scenario can't be built
        }
        assert!(result.is_err(), "unreadable subdir must error, not read as deletions");
    }

    #[test]
    fn rel_to_base_strips_prefix() {
        assert_eq!(rel_to_base("/Music", "/Music/sub/"), "sub");
        assert_eq!(rel_to_base("/Music", "/Music/song.mp3"), "song.mp3");
        assert_eq!(rel_to_base("/Music", "/Music/a/b.txt"), "a/b.txt");
        // Root folder: base normalizes to "/".
        assert_eq!(rel_to_base("/", "/Foo"), "Foo");
    }

    #[test]
    fn remote_join_builds_paths() {
        assert_eq!(remote_join("/Music", "sub/song.mp3"), "/Music/sub/song.mp3");
        assert_eq!(remote_join("/", "song.mp3"), "/song.mp3");
        assert_eq!(remote_join("/Music", "song.mp3"), "/Music/song.mp3");
    }

    /// Full bidirectional sync against a real Nextcloud. Ignored by default:
    ///   NC_URL=.. NC_USER=.. NC_PASS=.. cargo test live_sync -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_sync_bidirectional() {
        use crate::config::{Account, ServerKind, SyncFolder};
        use crate::webdav::WebDavClient;
        use std::time::{SystemTime, UNIX_EPOCH};

        let account = Account::new(
            std::env::var("NC_URL").unwrap(),
            std::env::var("NC_USER").unwrap(),
            ServerKind::Nextcloud,
        );
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let local = std::env::temp_dir().join(format!("nckde_local_{uniq}"));
        let jdir = std::env::temp_dir().join(format!("nckde_journal_{uniq}"));
        std::fs::create_dir_all(&local).unwrap();
        let remote = format!("/nc_kde_synctest_{uniq}");
        let folder = SyncFolder {
            id: "test".into(),
            account_id: account.id.clone(),
            local_path: local.to_string_lossy().into_owned(),
            remote_path: remote.clone(),
            enabled: true,
        };
        let rp = |p: &str| format!("{remote}/{p}");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let reporter = Reporter::new(tx);
        let _ = client.delete(&remote).await;

        // 1. Local → remote upload (incl. a nested file).
        std::fs::write(local.join("a.txt"), b"one").unwrap();
        std::fs::create_dir_all(local.join("sub")).unwrap();
        std::fs::write(local.join("sub/b.txt"), b"two").unwrap();
        println!("run1: {:?}", sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap());
        assert!(client.stat(&rp("a.txt")).await.unwrap().is_some(), "a.txt uploaded");
        assert!(client.stat(&rp("sub/b.txt")).await.unwrap().is_some(), "nested uploaded");

        // 2. Remote → local download.
        client.put_bytes(&rp("c.txt"), b"three".to_vec()).await.unwrap();
        println!("run2: {:?}", sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap());
        assert_eq!(std::fs::read(local.join("c.txt")).unwrap(), b"three", "c.txt downloaded");

        // 3. Local deletion → remote deletion.
        std::fs::remove_file(local.join("a.txt")).unwrap();
        println!("run3: {:?}", sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap());
        assert!(client.stat(&rp("a.txt")).await.unwrap().is_none(), "a.txt deleted on server");

        // 4. Remote deletion → local deletion.
        client.delete(&rp("sub/b.txt")).await.unwrap();
        println!("run4: {:?}", sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap());
        assert!(!local.join("sub/b.txt").exists(), "b.txt deleted locally");

        // 5. Idempotency: a clean run does nothing.
        let s5 = sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap();
        println!("run5 (noop): {s5:?}");
        assert_eq!(s5.uploaded + s5.downloaded + s5.deleted_local + s5.deleted_remote, 0);

        // 6. Identical new file on BOTH sides → adopted, not a conflict.
        std::fs::write(local.join("same.txt"), b"identical").unwrap();
        client.put_bytes(&rp("same.txt"), b"identical".to_vec()).await.unwrap();
        let s6 = sync_folder(&jdir, &client, &folder, &reporter, &[]).await.unwrap();
        println!("run6 (identical both sides): {s6:?}");
        assert_eq!(s6.conflicts, 0, "identical content is not a conflict");
        let has_conflict_copy = std::fs::read_dir(&local)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("conflicted copy"));
        assert!(!has_conflict_copy, "no conflicted-copy file created");

        let _ = client.delete(&remote).await;
        let _ = std::fs::remove_dir_all(&local);
        let _ = std::fs::remove_dir_all(&jdir);
        println!("LIVE SYNC OK");
    }

    // ---- Live scenario suite -------------------------------------------------
    // Assertion-based, #[ignore] by default (need a reachable Nextcloud). Run
    // the whole suite via packaging/live-tests.sh, or manually:
    //   NC_URL=.. NC_USER=.. NC_PASS=.. cargo test live_ -- --ignored --nocapture
    // Each test uses a unique remote path + journal dir and cleans up after
    // itself, so they are independent and can run in any order.

    struct LiveEnv {
        client: WebDavClient,
        folder: SyncFolder,
        local: PathBuf,
        jdir: PathBuf,
        remote: String,
        reporter: Reporter,
        // Keep the event receiver alive so Reporter sends don't error.
        _rx: tokio::sync::mpsc::UnboundedReceiver<crate::sync::progress::SyncEvent>,
    }

    impl LiveEnv {
        fn new(tag: &str) -> Self {
            use crate::config::{Account, ServerKind};
            use std::time::{SystemTime, UNIX_EPOCH};
            let account = Account::new(
                std::env::var("NC_URL").expect("NC_URL"),
                std::env::var("NC_USER").expect("NC_USER"),
                ServerKind::Nextcloud,
            );
            let client =
                WebDavClient::new(&account, std::env::var("NC_PASS").expect("NC_PASS")).unwrap();
            let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let local = std::env::temp_dir().join(format!("cirlive_{tag}_{uniq}"));
            let jdir = std::env::temp_dir().join(format!("cirlive_j_{tag}_{uniq}"));
            std::fs::create_dir_all(&local).unwrap();
            let remote = format!("/cirlive_{tag}_{uniq}");
            let folder = SyncFolder {
                id: format!("live-{tag}"),
                account_id: account.id.clone(),
                local_path: local.to_string_lossy().into_owned(),
                remote_path: remote.clone(),
                enabled: true,
            };
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            Self { client, folder, local, jdir, remote, reporter: Reporter::new(tx), _rx }
        }
        fn rp(&self, p: &str) -> String {
            format!("{}/{}", self.remote, p)
        }
        fn lp(&self, p: &str) -> PathBuf {
            self.local.join(p)
        }
        async fn sync(&self) -> SyncStats {
            self.sync_ignore(&[]).await
        }
        async fn sync_ignore(&self, ig: &[String]) -> SyncStats {
            let stats = sync_folder(&self.jdir, &self.client, &self.folder, &self.reporter, ig)
                .await
                .unwrap();
            // Nextcloud file ETags and local mtimes are second-granular. A
            // test mutation issued immediately after a sync can land in the
            // same second as the state the journal just recorded, making the
            // change genuinely undetectable — the scenario then races on a
            // fast machine (seen as sporadic single-test failures). Step past
            // the second boundary before handing control back.
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            stats
        }
        async fn r_exists(&self, p: &str) -> bool {
            self.client.stat(&self.rp(p)).await.unwrap().is_some()
        }
        async fn r_bytes(&self, p: &str) -> Vec<u8> {
            self.client.get_bytes(&self.rp(p)).await.unwrap()
        }
        async fn cleanup(self) {
            let _ = self.client.delete(&self.remote).await;
            let _ = std::fs::remove_dir_all(&self.local);
            let _ = std::fs::remove_dir_all(&self.jdir);
        }
    }

    /// A file changed on one side propagates to the other (both directions).
    #[tokio::test]
    #[ignore]
    async fn live_file_modify() {
        let e = LiveEnv::new("filemod");
        std::fs::write(e.lp("f.txt"), b"v1").unwrap();
        e.sync().await;
        assert_eq!(e.r_bytes("f.txt").await, b"v1");

        // Local edit -> uploaded.
        std::fs::write(e.lp("f.txt"), b"v2-local-edit").unwrap();
        e.sync().await;
        assert_eq!(e.r_bytes("f.txt").await, b"v2-local-edit", "local edit uploaded");

        // Remote edit -> downloaded.
        e.client.put_bytes(&e.rp("f.txt"), b"v3-remote-edit".to_vec()).await.unwrap();
        e.sync().await;
        assert_eq!(std::fs::read(e.lp("f.txt")).unwrap(), b"v3-remote-edit", "remote edit downloaded");

        e.cleanup().await;
        println!("live_file_modify OK");
    }

    /// Asserts a diverging edit produced a conflict: the server version is taken
    /// for `name`, and the local edit survives as a "conflicted copy" alongside.
    async fn assert_conflicted(e: &LiveEnv, name: &str, remote_bytes: &[u8], local_bytes: &[u8]) {
        assert_eq!(std::fs::read(e.lp(name)).unwrap(), remote_bytes, "remote version taken");
        let copy = std::fs::read_dir(&e.local)
            .unwrap()
            .filter_map(|x| x.ok())
            .find(|x| x.file_name().to_string_lossy().contains("conflicted copy"));
        assert!(copy.is_some(), "local edit preserved as a conflicted copy");
        assert_eq!(std::fs::read(copy.unwrap().path()).unwrap(), local_bytes, "conflicted copy holds local edit");
    }

    /// Diverging edit, DIFFERENT sizes → the fast path (no content compare):
    /// conflicted copy + server version.
    #[tokio::test]
    #[ignore]
    async fn live_file_conflict_diff_size() {
        let e = LiveEnv::new("conflictdiff");
        std::fs::write(e.lp("f.txt"), b"base").unwrap();
        e.sync().await;

        std::fs::write(e.lp("f.txt"), b"local side change").unwrap(); // 17 bytes
        e.client.put_bytes(&e.rp("f.txt"), b"remote".to_vec()).await.unwrap(); // 6 bytes
        let s = e.sync().await;
        assert_eq!(s.conflicts, 1, "one conflict recorded");
        assert_conflicted(&e, "f.txt", b"remote", b"local side change").await;

        e.cleanup().await;
        println!("live_file_conflict_diff_size OK");
    }

    /// Diverging edit, SAME size → the content-compare path. This is the subtle
    /// boundary: same-size same-content is adopted silently, but same-size
    /// DIFFERENT content must still be a conflict, not a silent adoption that
    /// would quietly discard one side. Both edits are 8 bytes.
    #[tokio::test]
    #[ignore]
    async fn live_file_conflict_same_size() {
        let e = LiveEnv::new("conflictsame");
        std::fs::write(e.lp("f.txt"), b"original").unwrap(); // 8 bytes
        e.sync().await;

        std::fs::write(e.lp("f.txt"), b"LOCALvvv").unwrap(); // 8 bytes
        e.client.put_bytes(&e.rp("f.txt"), b"remotexx".to_vec()).await.unwrap(); // 8 bytes
        let s = e.sync().await;
        assert_eq!(s.conflicts, 1, "same-size divergence must still conflict, not adopt");
        assert_conflicted(&e, "f.txt", b"remotexx", b"LOCALvvv").await;

        e.cleanup().await;
        println!("live_file_conflict_same_size OK");
    }

    /// Same size AND identical content on both sides is adopted silently — no
    /// conflict, no conflicted copy. Complements the same-size divergence test.
    #[tokio::test]
    #[ignore]
    async fn live_file_same_size_identical_adopted() {
        let e = LiveEnv::new("adopt");
        std::fs::write(e.lp("f.txt"), b"seed").unwrap();
        e.sync().await;

        // Identical 9-byte content written to both sides independently.
        std::fs::write(e.lp("f.txt"), b"identical").unwrap();
        e.client.put_bytes(&e.rp("f.txt"), b"identical".to_vec()).await.unwrap();
        let s = e.sync().await;
        assert_eq!(s.conflicts, 0, "identical content is not a conflict");
        let has_copy = std::fs::read_dir(&e.local)
            .unwrap()
            .filter_map(|x| x.ok())
            .any(|x| x.file_name().to_string_lossy().contains("conflicted copy"));
        assert!(!has_copy, "no conflicted copy for identical content");
        assert_eq!(std::fs::read(e.lp("f.txt")).unwrap(), b"identical");

        e.cleanup().await;
        println!("live_file_same_size_identical_adopted OK");
    }

    /// Delete-vs-modify: the surviving change wins over the deletion, both ways.
    #[tokio::test]
    #[ignore]
    async fn live_delete_vs_modify() {
        // (a) local edits, remote deletes -> local edit wins (file restored remotely)
        let e = LiveEnv::new("delmod_a");
        std::fs::write(e.lp("f.txt"), b"base").unwrap();
        e.sync().await;
        std::fs::write(e.lp("f.txt"), b"local-edit-wins").unwrap();
        e.client.delete(&e.rp("f.txt")).await.unwrap();
        e.sync().await;
        assert!(e.r_exists("f.txt").await, "locally-edited file restored on the server");
        assert_eq!(e.r_bytes("f.txt").await, b"local-edit-wins");
        assert_eq!(std::fs::read(e.lp("f.txt")).unwrap(), b"local-edit-wins");
        e.cleanup().await;

        // (b) local deletes, remote edits -> remote edit wins (file restored locally)
        let e = LiveEnv::new("delmod_b");
        std::fs::write(e.lp("f.txt"), b"base").unwrap();
        e.sync().await;
        std::fs::remove_file(e.lp("f.txt")).unwrap();
        e.client.put_bytes(&e.rp("f.txt"), b"remote-edit-wins".to_vec()).await.unwrap();
        e.sync().await;
        assert!(e.lp("f.txt").exists(), "remotely-edited file restored locally");
        assert_eq!(std::fs::read(e.lp("f.txt")).unwrap(), b"remote-edit-wins");
        assert!(e.r_exists("f.txt").await);
        e.cleanup().await;
        println!("live_delete_vs_modify OK");
    }

    /// Empty directories propagate in both directions — creation and deletion.
    #[tokio::test]
    #[ignore]
    async fn live_dir_empty() {
        let e = LiveEnv::new("direfempty");
        std::fs::create_dir_all(e.lp("d")).unwrap();
        e.sync().await;
        assert!(e.r_exists("d").await, "empty dir created remotely");

        std::fs::remove_dir(e.lp("d")).unwrap();
        e.sync().await;
        assert!(!e.r_exists("d").await, "empty dir deleted remotely");

        // Remote-created empty dir -> local.
        e.client.mkcol(&e.rp("r")).await.unwrap();
        e.sync().await;
        assert!(e.lp("r").is_dir(), "remote empty dir created locally");

        e.client.delete(&e.rp("r")).await.unwrap();
        e.sync().await;
        assert!(!e.lp("r").exists(), "remote empty dir deleted locally");

        e.cleanup().await;
        println!("live_dir_empty OK");
    }

    /// The fix: deleting a directory that *held files* removes it on the other
    /// side instead of leaving an empty ghost that gets re-created.
    #[tokio::test]
    #[ignore]
    async fn live_dir_nonempty_delete() {
        // (a) local deletion -> remote folder gone
        let e = LiveEnv::new("dirdel_l");
        std::fs::create_dir_all(e.lp("d")).unwrap();
        std::fs::write(e.lp("d/x.txt"), b"x").unwrap();
        e.sync().await;
        assert!(e.r_exists("d/x.txt").await && e.r_exists("d").await);

        std::fs::remove_dir_all(e.lp("d")).unwrap();
        e.sync().await;
        assert!(!e.r_exists("d/x.txt").await, "child gone remotely");
        assert!(!e.r_exists("d").await, "FOLDER gone remotely (was the bug)");
        assert!(!e.lp("d").exists(), "folder not re-created locally");
        // A second run is a no-op (converged).
        let s2 = e.sync().await;
        assert_eq!(s2.deleted_local + s2.deleted_remote + s2.uploaded + s2.downloaded, 0);
        e.cleanup().await;

        // (b) remote deletion -> local folder gone
        let e = LiveEnv::new("dirdel_r");
        std::fs::create_dir_all(e.lp("d")).unwrap();
        std::fs::write(e.lp("d/x.txt"), b"x").unwrap();
        e.sync().await;
        e.client.delete(&e.rp("d")).await.unwrap(); // recursive on server
        e.sync().await;
        assert!(!e.lp("d/x.txt").exists(), "child gone locally");
        assert!(!e.lp("d").exists(), "FOLDER gone locally");
        assert!(!e.r_exists("d").await, "folder not re-created remotely");
        e.cleanup().await;
        println!("live_dir_nonempty_delete OK");
    }

    /// Local folder deleted, but a *new* file was added inside it remotely: the
    /// folder must be preserved (the new file downloaded), not swept away.
    #[tokio::test]
    #[ignore]
    async fn live_dir_preserve_remote_addition() {
        let e = LiveEnv::new("dirpreserve");
        std::fs::create_dir_all(e.lp("d")).unwrap();
        std::fs::write(e.lp("d/old.txt"), b"old").unwrap();
        e.sync().await;

        // Delete the folder locally; independently add a file into it remotely.
        std::fs::remove_dir_all(e.lp("d")).unwrap();
        e.client.put_bytes(&e.rp("d/new.txt"), b"new".to_vec()).await.unwrap();
        e.sync().await;

        assert!(e.r_exists("d").await, "folder preserved remotely (holds a new file)");
        assert!(!e.r_exists("d/old.txt").await, "the file the user deleted is gone");
        assert!(e.r_exists("d/new.txt").await, "remotely-added file kept");
        assert_eq!(std::fs::read(e.lp("d/new.txt")).unwrap(), b"new", "new file downloaded locally");

        e.cleanup().await;
        println!("live_dir_preserve_remote_addition OK");
    }

    /// A nested tree deleted at its root propagates the whole subtree.
    #[tokio::test]
    #[ignore]
    async fn live_dir_nested_delete() {
        let e = LiveEnv::new("nested");
        std::fs::create_dir_all(e.lp("a/b/c")).unwrap();
        std::fs::write(e.lp("a/top.txt"), b"1").unwrap();
        std::fs::write(e.lp("a/b/mid.txt"), b"2").unwrap();
        std::fs::write(e.lp("a/b/c/deep.txt"), b"3").unwrap();
        e.sync().await;
        assert!(e.r_exists("a/b/c/deep.txt").await);

        std::fs::remove_dir_all(e.lp("a")).unwrap();
        e.sync().await;
        for p in ["a", "a/b", "a/b/c", "a/top.txt", "a/b/mid.txt", "a/b/c/deep.txt"] {
            assert!(!e.r_exists(p).await, "{p} gone remotely");
        }
        e.cleanup().await;
        println!("live_dir_nested_delete OK");
    }

    /// Ignored paths are neither uploaded nor deleted.
    #[tokio::test]
    #[ignore]
    async fn live_ignore_patterns() {
        let e = LiveEnv::new("ignore");
        let ig = vec!["*.tmp".to_string(), "node_modules".to_string()];
        std::fs::write(e.lp("keep.txt"), b"k").unwrap();
        std::fs::write(e.lp("scratch.tmp"), b"t").unwrap();
        std::fs::create_dir_all(e.lp("node_modules")).unwrap();
        std::fs::write(e.lp("node_modules/lib.js"), b"j").unwrap();
        e.sync_ignore(&ig).await;

        assert!(e.r_exists("keep.txt").await, "normal file uploaded");
        assert!(!e.r_exists("scratch.tmp").await, "*.tmp not uploaded");
        assert!(!e.r_exists("node_modules").await, "ignored dir not uploaded");

        e.cleanup().await;
        println!("live_ignore_patterns OK");
    }

    /// Pairing a folder that already holds identical content on BOTH sides (a
    /// first sync with no journal): everything is adopted silently — no
    /// conflicts, no conflicted copies, no re-transfer, and a clean second run.
    #[tokio::test]
    #[ignore]
    async fn live_first_sync_preexisting_identical() {
        let e = LiveEnv::new("adoptfolder");
        // Local tree …
        std::fs::create_dir_all(e.lp("docs")).unwrap();
        std::fs::write(e.lp("top.txt"), b"top-content").unwrap();
        std::fs::write(e.lp("docs/note.txt"), b"note-content").unwrap();
        // … and the very same tree already on the server, before first sync.
        // The pairing targets an existing remote folder, so create the root too.
        e.client.mkcol(&e.remote).await.unwrap();
        e.client.mkcol(&e.rp("docs")).await.unwrap();
        e.client.put_bytes(&e.rp("top.txt"), b"top-content".to_vec()).await.unwrap();
        e.client.put_bytes(&e.rp("docs/note.txt"), b"note-content".to_vec()).await.unwrap();

        let s = e.sync().await;
        assert_eq!(s.conflicts, 0, "identical pre-existing content is adopted, not a conflict");
        assert_eq!(s.uploaded + s.downloaded, 0, "nothing re-transferred");
        let any_copy = std::fs::read_dir(&e.local)
            .unwrap()
            .filter_map(|x| x.ok())
            .any(|x| x.file_name().to_string_lossy().contains("conflicted copy"));
        assert!(!any_copy, "no conflicted copies created");
        // Both sides still intact.
        assert_eq!(e.r_bytes("docs/note.txt").await, b"note-content");
        assert_eq!(std::fs::read(e.lp("top.txt")).unwrap(), b"top-content");
        // Second run is a clean no-op — proves the adoption was journaled.
        let s2 = e.sync().await;
        assert_eq!(
            s2.uploaded + s2.downloaded + s2.deleted_local + s2.deleted_remote + s2.conflicts,
            0,
            "converged"
        );

        e.cleanup().await;
        println!("live_first_sync_preexisting_identical OK");
    }

    /// Regression for the reported data-loss bug: pairing a local folder with
    /// a same-name server folder holding DIFFERENT files must merge the two
    /// sides into a union — and must never delete anything on either side.
    #[tokio::test]
    #[ignore]
    async fn live_first_sync_preexisting_divergent_merge() {
        let e = LiveEnv::new("merge");
        // Local tree…
        std::fs::create_dir_all(e.lp("sub")).unwrap();
        std::fs::write(e.lp("local-only.txt"), b"mine").unwrap();
        std::fs::write(e.lp("sub/nested-local.txt"), b"mine2").unwrap();
        // …and a pre-existing remote tree with entirely different content.
        e.client.mkcol(&e.remote).await.unwrap();
        e.client.mkcol(&e.rp("docs")).await.unwrap();
        e.client.put_bytes(&e.rp("server-only.txt"), b"theirs".to_vec()).await.unwrap();
        e.client.put_bytes(&e.rp("docs/nested-server.txt"), b"theirs2".to_vec()).await.unwrap();

        let s = e.sync().await;
        assert_eq!(s.deleted_local + s.deleted_remote, 0, "a first sync must never delete");
        let union = ["local-only.txt", "sub/nested-local.txt", "server-only.txt", "docs/nested-server.txt"];
        for p in union {
            assert!(e.r_exists(p).await, "{p} present on the server");
            assert!(e.lp(p).exists(), "{p} present locally");
        }
        assert_eq!(e.r_bytes("local-only.txt").await, b"mine");
        assert_eq!(std::fs::read(e.lp("server-only.txt")).unwrap(), b"theirs");
        // And a second run is a clean no-op.
        let s2 = e.sync().await;
        assert_eq!(
            s2.uploaded + s2.downloaded + s2.deleted_local + s2.deleted_remote + s2.conflicts,
            0,
            "converged"
        );

        e.cleanup().await;
        println!("live_first_sync_preexisting_divergent_merge OK");
    }

    /// State loss must read as "restore", never "delete": when the entire
    /// local folder disappears (unmount, rename, deletion outside the app),
    /// the mass-deletion guard refuses the server-side sweep, and the next run
    /// restores the tree locally instead.
    #[tokio::test]
    #[ignore]
    async fn live_local_folder_lost_restores() {
        let e = LiveEnv::new("lostroot");
        for i in 0..12 {
            std::fs::write(e.lp(&format!("f{i:02}.txt")), format!("data-{i}").into_bytes()).unwrap();
        }
        let s = e.sync().await;
        assert_eq!(s.uploaded, 12);

        // The local folder vanishes wholesale (prepare() re-creates it empty).
        std::fs::remove_dir_all(&e.local).unwrap();
        let s2 = e.sync().await;
        assert_eq!(s2.deleted_remote, 0, "guard refused the deletion sweep");
        assert_eq!(s2.blocked_deletions, 12);
        for i in 0..12 {
            assert!(e.r_exists(&format!("f{i:02}.txt")).await, "f{i:02} survived on the server");
        }

        // Next run: the guarded files' journal entries were dropped, so the
        // server copies count as new and are restored locally.
        let s3 = e.sync().await;
        assert_eq!(s3.downloaded, 12, "server files restored locally");
        for i in 0..12 {
            assert!(e.lp(&format!("f{i:02}.txt")).exists(), "f{i:02} restored locally");
        }

        e.cleanup().await;
        println!("live_local_folder_lost_restores OK");
    }

    /// Mirror of the lost-local case: the entire REMOTE folder disappears
    /// (deleted server-side, out from under the pair). The guard refuses the
    /// local deletion sweep, and the next run restores the tree to the server.
    #[tokio::test]
    #[ignore]
    async fn live_remote_folder_lost_restores() {
        let e = LiveEnv::new("lostremote");
        for i in 0..12 {
            std::fs::write(e.lp(&format!("f{i:02}.txt")), format!("data-{i}").into_bytes()).unwrap();
        }
        assert_eq!(e.sync().await.uploaded, 12);

        // The remote collection vanishes wholesale (prepare() re-creates it empty).
        e.client.delete(&e.remote).await.unwrap();
        let s2 = e.sync().await;
        assert_eq!(s2.deleted_local, 0, "guard refused the local deletion sweep");
        assert_eq!(s2.blocked_deletions, 12);
        for i in 0..12 {
            assert!(e.lp(&format!("f{i:02}.txt")).exists(), "f{i:02} kept locally");
        }

        // Next run: the guarded files' journal entries were dropped, so the
        // local copies count as new and are restored to the server.
        let s3 = e.sync().await;
        assert_eq!(s3.uploaded, 12, "local files restored to the server");
        for i in 0..12 {
            assert!(e.r_exists(&format!("f{i:02}.txt")).await, "f{i:02} restored remotely");
        }

        e.cleanup().await;
        println!("live_remote_folder_lost_restores OK");
    }

    /// Adding a folder pair whose remote name is already taken by a populated
    /// folder must NOT pair with it — it becomes a second version ("name 2"),
    /// and the pre-existing server data stays untouched.
    #[tokio::test]
    #[ignore]
    async fn live_add_folder_dedupes_existing_remote() {
        let e = LiveEnv::new("dedupe");
        // Pre-existing, populated server folder under the desired name.
        e.client.mkcol(&e.remote).await.unwrap();
        e.client.put_bytes(&e.rp("precious.txt"), b"server data".to_vec()).await.unwrap();

        let unique = unique_remote_path(&e.client, &e.remote).await.unwrap();
        assert_eq!(unique, format!("{} 2", e.remote), "occupied name gets a ' 2' suffix");

        // Sync the local folder into the deduped path; the original is untouched.
        std::fs::write(e.lp("mine.txt"), b"local data").unwrap();
        let mut folder = e.folder.clone();
        folder.remote_path = unique.clone();
        sync_folder(&e.jdir, &e.client, &folder, &e.reporter, &[]).await.unwrap();
        assert_eq!(e.r_bytes("precious.txt").await, b"server data", "pre-existing data untouched");
        assert!(
            e.client.stat(&format!("{unique}/mine.txt")).await.unwrap().is_some(),
            "local file synced into the deduped folder"
        );

        // An EMPTY existing folder is reused as-is; a free name passes through.
        let empty = format!("{}_empty", e.remote);
        e.client.mkcol(&empty).await.unwrap();
        assert_eq!(unique_remote_path(&e.client, &empty).await.unwrap(), empty);
        let fresh = format!("{}_fresh", e.remote);
        assert_eq!(unique_remote_path(&e.client, &fresh).await.unwrap(), fresh);
        // A FILE occupying the name is stepped over too.
        let taken_by_file = format!("{}_file", e.remote);
        e.client.put_bytes(&taken_by_file, b"i am a file".to_vec()).await.unwrap();
        assert_eq!(
            unique_remote_path(&e.client, &taken_by_file).await.unwrap(),
            format!("{taken_by_file} 2"),
            "a file with the desired name forces the suffix"
        );

        let _ = e.client.delete(&unique).await;
        let _ = e.client.delete(&empty).await;
        let _ = e.client.delete(&taken_by_file).await;
        e.cleanup().await;
        println!("live_add_folder_dedupes_existing_remote OK");
    }

    /// Pausing a running sync: a cancelled scan aborts (a partial tree must
    /// never be planned against), a cancelled execution transfers nothing, and
    /// the next uncancelled run picks the work up exactly where it stopped.
    #[tokio::test]
    #[ignore]
    async fn live_cancel_stops_run_and_resumes() {
        let e = LiveEnv::new("cancel");
        std::fs::write(e.lp("a.txt"), b"payload-a").unwrap();
        std::fs::write(e.lp("b.txt"), b"payload-b").unwrap();

        let cancelled = Cancel::new(|| true);
        let idle = Cancel::never();

        // Cancelled during the scan → error, no plan.
        assert!(
            prepare(&e.jdir, &e.client, &e.folder, &[], &e.reporter, &cancelled).await.is_err(),
            "a cancelled walk must not produce a (partial) plan"
        );

        // Cancelled during execution → nothing transferred.
        let plan =
            prepare(&e.jdir, &e.client, &e.folder, &[], &e.reporter, &idle).await.unwrap();
        assert_eq!(plan.files_total, 2);
        let s = sync_prepared(&e.jdir, &e.client, &e.folder, plan, &e.reporter, &[], &cancelled)
            .await
            .unwrap();
        assert_eq!(
            s.uploaded + s.downloaded + s.deleted_local + s.deleted_remote,
            0,
            "cancelled run transfers nothing"
        );
        assert!(!e.r_exists("a.txt").await, "no upload slipped through");

        // The next uncancelled run completes the work.
        let s2 = e.sync().await;
        assert_eq!(s2.uploaded, 2, "both files uploaded after resume");
        assert_eq!(e.r_bytes("a.txt").await, b"payload-a");
        assert_eq!(e.r_bytes("b.txt").await, b"payload-b");

        e.cleanup().await;
        println!("live_cancel_stops_run_and_resumes OK");
    }

    /// Pausing must not corrupt a pending deletion: a delete planned but
    /// cancelled keeps its journal entry, so the next run finishes the delete
    /// instead of resurrecting the file from the server.
    #[tokio::test]
    #[ignore]
    async fn live_cancel_preserves_pending_deletion() {
        let e = LiveEnv::new("canceldel");
        std::fs::write(e.lp("f.txt"), b"to-be-deleted").unwrap();
        e.sync().await;
        assert!(e.r_exists("f.txt").await);

        // Delete locally, then let the propagating run get cancelled right
        // before the deletion pass executes.
        std::fs::remove_file(e.lp("f.txt")).unwrap();
        let idle = Cancel::never();
        let plan =
            prepare(&e.jdir, &e.client, &e.folder, &[], &e.reporter, &idle).await.unwrap();
        let cancelled = Cancel::new(|| true);
        sync_prepared(&e.jdir, &e.client, &e.folder, plan, &e.reporter, &[], &cancelled)
            .await
            .unwrap();
        assert!(e.r_exists("f.txt").await, "cancelled run must not have deleted remotely yet");
        assert!(!e.lp("f.txt").exists(), "…nor resurrected the file locally");

        // The resumed run finishes the deletion (the regression would be a
        // re-download here, because the journal entry was dropped).
        let s = e.sync().await;
        assert_eq!(s.downloaded, 0, "file must not be resurrected by re-download");
        assert!(!e.lp("f.txt").exists());
        assert!(!e.r_exists("f.txt").await, "deletion completed after resume");

        e.cleanup().await;
        println!("live_cancel_preserves_pending_deletion OK");
    }

    // ---- CalDAV / CardDAV -----------------------------------------------------
    // Exercise the raw DAV layer (dav.rs) against Nextcloud's PIM endpoints. The
    // codec (contentline/vcard/ical) is unit-tested separately; the live value
    // here is the real round-trip and — crucially — the ETag guard that stops a
    // stale write from clobbering a concurrent change. Needs the default
    // `contacts` addressbook and `personal` calendar (live-tests.sh ensures them).

    fn live_client() -> WebDavClient {
        use crate::config::{Account, ServerKind};
        let account = Account::new(
            std::env::var("NC_URL").expect("NC_URL"),
            std::env::var("NC_USER").expect("NC_USER"),
            ServerKind::Nextcloud,
        );
        WebDavClient::new(&account, std::env::var("NC_PASS").expect("NC_PASS")).unwrap()
    }

    fn uniq_tag() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    }

    /// Re-read the ETag when a PUT response omitted the header.
    async fn etag_of(c: &WebDavClient, path: &str, from_put: String) -> String {
        if from_put.is_empty() {
            c.dav_fetch_etag(path).await.unwrap()
        } else {
            from_put
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_carddav_crud() {
        let c = live_client();
        let user = c.user().to_string();
        let uid = format!("cirtest-{}", uniq_tag());
        let path = format!("addressbooks/users/{user}/contacts/{uid}.vcf");
        let ct = "text/vcard; charset=utf-8";
        let card = |email: &str| {
            format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Test Person\r\n\
                 N:Person;Test;;;\r\nEMAIL;TYPE=WORK:{email}\r\n\
                 X-CIRRUST-CUSTOM:preserve-me\r\nEND:VCARD\r\n"
            )
        };

        // Create → read back → custom property survives the server round-trip.
        let e1 = etag_of(&c, &path, c.dav_put_new(&path, ct, card("a@example.org")).await.unwrap()).await;
        let (_, body) = c.dav_get_item(&path).await.unwrap();
        assert!(body.contains("FN:Test Person"), "vCard stored");
        assert!(body.contains("X-CIRRUST-CUSTOM:preserve-me"), "custom property preserved");

        // Update guarded by the current ETag → succeeds and bumps the ETag.
        let e2 = etag_of(&c, &path, c.dav_put_update(&path, ct, card("b@example.org"), &e1).await.unwrap()).await;
        assert_ne!(e1, e2, "ETag changed after update");

        // The anti-clobber guard: an update with the STALE ETag is rejected.
        match c.dav_put_update(&path, ct, card("c@example.org"), &e1).await {
            Err(AppError::Server { status: 412, .. }) => {}
            other => panic!("stale If-Match must be a 412, got {other:?}"),
        }

        c.dav_delete_item(&path, &e2).await.unwrap();
        assert!(c.dav_get_item(&path).await.is_err(), "contact deleted");
        println!("live_carddav_crud OK");
    }

    #[tokio::test]
    #[ignore]
    async fn live_caldav_crud() {
        let c = live_client();
        let user = c.user().to_string();
        let uid = format!("cirtest-{}", uniq_tag());
        let path = format!("calendars/{user}/personal/{uid}.ics");
        let ct = "text/calendar; charset=utf-8";
        let ev = |summary: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Cirrust//live test//EN\r\n\
                 BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
                 DTSTART:20260201T100000Z\r\nDTEND:20260201T110000Z\r\n\
                 SUMMARY:{summary}\r\nX-CIRRUST-CUSTOM:keep-this\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };

        let e1 = etag_of(&c, &path, c.dav_put_new(&path, ct, ev("Live test event")).await.unwrap()).await;
        let (_, body) = c.dav_get_item(&path).await.unwrap();
        assert!(body.contains("SUMMARY:Live test event"), "event stored");
        assert!(body.contains("X-CIRRUST-CUSTOM:keep-this"), "custom property preserved");

        let e2 = etag_of(&c, &path, c.dav_put_update(&path, ct, ev("Edited event"), &e1).await.unwrap()).await;
        assert_ne!(e1, e2, "ETag changed after update");

        match c.dav_put_update(&path, ct, ev("Clobber"), &e1).await {
            Err(AppError::Server { status: 412, .. }) => {}
            other => panic!("stale If-Match must be a 412, got {other:?}"),
        }

        c.dav_delete_item(&path, &e2).await.unwrap();
        assert!(c.dav_get_item(&path).await.is_err(), "event deleted");
        println!("live_caldav_crud OK");
    }

    #[test]
    fn conflict_naming_roundtrips() {
        let c = conflicted_name(Path::new("/data/song.mp3"));
        let s = c.to_string_lossy();
        assert!(s.contains("song (conflicted copy"));
        assert!(s.ends_with(".mp3"));
        assert!(is_conflicted(&s.rsplit('/').next().unwrap()));
        assert!(!is_conflicted("ordinary.txt"));
    }

    #[test]
    fn wildcard_and_ignore() {
        assert!(wildcard_match(b"*.tmp", b"foo.tmp"));
        assert!(!wildcard_match(b"*.tmp", b"foo.txt"));
        assert!(wildcard_match(b"a?c", b"abc"));
        assert!(wildcard_match(b"*", b"anything"));
        assert!(wildcard_match(b"node_modules", b"node_modules"));

        let pats = vec!["*.tmp".to_string(), "node_modules".to_string(), ".git".to_string()];
        assert!(is_ignored("a/b/c.tmp", &pats)); // segment *.tmp
        assert!(is_ignored("node_modules/react/index.js", &pats)); // dir + children
        assert!(is_ignored(".git", &pats));
        assert!(!is_ignored("src/main.rs", &pats));
    }

    #[test]
    fn conflict_original_recovers_name() {
        assert_eq!(
            conflict_original("song (conflicted copy 2026-07-05 120000).mp3").as_deref(),
            Some("song.mp3"),
        );
        assert_eq!(conflict_original("normal.mp3"), None);
    }
}
