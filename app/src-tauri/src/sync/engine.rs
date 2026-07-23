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
use walkdir::WalkDir;

/// The action chosen for a path after comparing remote / local / journal.
#[derive(Clone, Copy, PartialEq)]
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
            let unchanged = j.map_or(false, |j| j.etag == r.etag && j.is_dir == r.is_dir);
            if j.is_some() && unchanged {
                Decision::DeleteRemote
            } else if r.is_dir {
                Decision::MkdirLocal
            } else {
                Decision::Download
            }
        }
        (None, Some(l)) => {
            let unchanged =
                j.map_or(false, |j| j.size == l.size && j.local_mtime == l.mtime && j.is_dir == l.is_dir);
            if j.is_some() && unchanged {
                Decision::DeleteLocal
            } else if l.is_dir {
                Decision::MkdirRemote
            } else {
                Decision::Upload
            }
        }
        (None, None) => Decision::None,
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncStats {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted_local: u32,
    pub deleted_remote: u32,
    pub conflicts: u32,
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
}

/// Walk both sides of a folder pair and tally what a sync would transfer.
/// Reports scan progress so the UI can show that something is happening.
pub async fn prepare(
    journal_dir: &Path,
    client: &WebDavClient,
    folder: &SyncFolder,
    ignore: &[String],
    reporter: &Reporter,
) -> AppResult<Prepared> {
    let local_root = PathBuf::from(&folder.local_path);
    tokio::fs::create_dir_all(&local_root).await?;
    ensure_remote_dir(client, &folder.remote_path).await?;

    let journal = Journal::load(journal_dir, &folder.id)?;
    let remote = walk_remote(client, &folder.remote_path, |found| {
        reporter.scan_progress(&folder.remote_path, found);
    })
    .await?;
    let local = walk_local(&local_root);

    // All paths seen anywhere, sorted so parents precede children.
    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(remote.keys().cloned());
    keys.extend(local.keys().cloned());
    keys.extend(journal.entries.keys().cloned());

    let (mut files_total, mut bytes_total) = (0u64, 0u64);
    for key in &keys {
        if is_ignored(key, ignore) {
            continue;
        }
        match classify(remote.get(key), local.get(key), journal.entries.get(key)) {
            Decision::Download | Decision::Conflict => {
                files_total += 1;
                bytes_total += remote.get(key).map_or(0, |r| r.size);
            }
            Decision::Upload => {
                files_total += 1;
                bytes_total += local.get(key).map_or(0, |l| l.size);
            }
            _ => {}
        }
    }

    Ok(Prepared { remote, local, journal, keys, files_total, bytes_total })
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
    let prepared = prepare(journal_dir, client, folder, ignore, reporter).await?;
    reporter.session_plan(prepared.files_total, prepared.bytes_total);
    sync_prepared(journal_dir, client, folder, prepared, reporter, ignore).await
}

/// Execute a previously [`prepare`]d sync. Returns per-run statistics.
pub async fn sync_prepared(
    journal_dir: &Path,
    client: &WebDavClient,
    folder: &SyncFolder,
    prepared: Prepared,
    reporter: &Reporter,
    ignore: &[String],
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
            async move {
                let weight = if item.size > LARGE_FILE_BYTES { 2 } else { 1 };
                let _permit = sem.acquire_many(weight).await.ok();
                let outcome = run_transfer(&client, &item, &reporter).await;
                (item, outcome)
            }
        }))
        // Poll more futures than the budget so freed permits are picked up
        // immediately; the semaphore is the real limiter.
        .buffer_unordered(TRANSFER_CONCURRENCY * 2);

        while let Some((item, outcome)) = stream.next().await {
            match outcome {
                Ok(entry) => {
                    match item.kind {
                        TransferKind::Upload => stats.uploaded += 1,
                        TransferKind::Download => stats.downloaded += 1,
                        TransferKind::ConflictDownload => stats.conflicts += 1,
                    }
                    new_journal.entries.insert(item.key.clone(), entry);
                }
                Err(e) => {
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
            async move {
                let identical = match checksum_matches(&item.local_full, &item.checksums).await
                {
                    Some(matched) => Ok(matched),
                    None => {
                        client
                            .compare_with_local(&item.remote_full, &item.local_full, |_| {})
                            .await
                    }
                };
                (item, identical)
            }
        }))
        .buffer_unordered(VERIFY_CONCURRENCY);

        while let Some((item, outcome)) = results.next().await {
            match outcome {
                Ok(true) => {
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
                Ok(false) => {
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
                Err(e) => {
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

    // Apply deletions child-first (reverse sorted order).
    for key in delete_remote.into_iter().rev() {
        if client.delete(&remote_join(&folder.remote_path, &key)).await.is_ok() {
            reporter.deleted(&key, true);
            stats.deleted_remote += 1;
        }
    }
    for key in delete_local.into_iter().rev() {
        let path = local_root.join(&key);
        let res = if path.is_dir() {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        if res.is_ok() {
            reporter.deleted(&key, false);
            stats.deleted_local += 1;
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

fn walk_local(root: &Path) -> HashMap<String, LocalMeta> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if rel.is_empty() || is_conflicted(&rel) || rel.ends_with(TMP_SUFFIX) {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
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
    out
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

fn remote_join(base: &str, rel: &str) -> String {
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
