//! A small, purpose-built WebDAV client for the Nextcloud `/remote.php/dav`
//! endpoint. Handles PROPFIND listing, GET/PUT, DELETE and MKCOL — the
//! primitives the file browser and the sync engine are built on.

use crate::config::Account;
use crate::error::{AppError, AppResult};
use futures_util::StreamExt;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Characters we keep verbatim inside a single path segment. Everything else
/// (including reserved chars) is percent-encoded; `/` is added back by the
/// caller when joining segments.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Hard cap on *metadata* round-trips (PROPFIND). These return a small,
/// bounded response, so unlike a file transfer they can be given a real total
/// timeout — which is what makes a dead link surface in a minute instead of
/// stalling behind the client's generous transfer inactivity window.
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Hard cap on the reachability probe. Deliberately short: it exists to answer
/// "is the server there right now?", and a slow answer is a no.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Chunk size for chunked uploads: 10 MiB. Small enough to clear typical server
/// request-size caps (Apache `LimitRequestBody`, PHP `post_max_size`) that would
/// otherwise reject a single large PUT with 413.
const UPLOAD_CHUNK_SIZE: usize = 10 * 1024 * 1024;

/// Files this size or larger go straight to chunked upload; smaller ones try a
/// single streamed PUT first (which the benchmark showed is markedly faster —
/// chunking adds a DELETE+MKCOL+MOVE round-trip per file) and only fall back to
/// chunking if the server rejects the whole-file PUT with 413. 100 MiB is a
/// safe bet to fit under any sane Nextcloud request-size cap, and at that size
/// the chunking overhead is negligible next to the transfer time anyway.
pub const CHUNK_UPLOAD_THRESHOLD: u64 = 100 * 1024 * 1024;

/// A single entry returned by a directory listing. Serialized to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Display name (last path segment).
    pub name: String,
    /// Path relative to the user's DAV root, always starting with `/`
    /// (e.g. `/Music/song.mp3`). Directories end with `/`.
    pub path: String,
    pub is_dir: bool,
    /// For directories this is the recursive size (`oc:size`) when available.
    pub size: u64,
    /// Last-modified time as an RFC 3339 string, if the server provided one.
    pub mtime: Option<String>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    /// Files contained in a directory (recursive), when the server reports it.
    pub file_count: Option<u64>,
    /// Sub-directories contained in a directory, when the server reports it.
    pub dir_count: Option<u64>,
    /// Server-stored checksums, e.g. `"SHA1:ab12… MD5:cd34…"` — present when
    /// the file was uploaded with one (files only).
    pub checksums: Option<String>,
}

/// Result of a (possibly ranged) GET, for the `stream://` protocol.
pub struct RangeGet {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_range: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct WebDavClient {
    http: Client,
    /// Server base, no trailing slash, e.g. `https://cloud.example.com`.
    server_url: String,
    /// `/remote.php/dav/files/<encoded-user>/` (leading + trailing slash).
    dav_root: String,
    username: String,
    password: String,
}

impl WebDavClient {
    pub fn new(account: &Account, password: String) -> AppResult<Self> {
        // We deliberately set NO total-request timeout: a transfer must never be
        // killed just because it's *big*. The only guards are:
        //
        //  * connect_timeout — bounds dead-host hangs during TCP/TLS setup only.
        //  * tcp_keepalive   — the kernel probes the peer, so a genuinely dead
        //    connection is detected (RST) regardless of the read_timeout below.
        //    This is what lets the inactivity window be long without ever
        //    hanging forever on a half-open socket.
        //  * read_timeout    — a *per-frame inactivity* timeout (reqwest resets
        //    it on every received data frame), so a transfer keeps running as
        //    long as bytes still flow; it only trips after a long silent gap.
        //    It's intentionally generous (15 min) because the wire can go quiet
        //    while the transfer is perfectly healthy — e.g. the server
        //    finalizing a large upload (assembling chunks, checksums, antivirus,
        //    the final MOVE) before it replies, or reconstructing a big file
        //    before the first download byte. A shorter window killed those even
        //    though the file was still uploading/downloading fine. Downloads
        //    resume from their partial temp and uploads restart against a remote
        //    temp, so even a real stall never corrupts or wastes committed work.
        //  * pool_idle_timeout — when the link dies without an RST (a VPN tunnel
        //    disappearing, a laptop suspending) every pooled socket is silently
        //    dead. Retiring idle connections quickly means the next request
        //    dials a fresh one and fails fast on `connect_timeout` instead of
        //    waiting out `read_timeout` on a black-holed socket.
        let http = Client::builder()
            .user_agent(crate::auth::DEVICE_NAME)
            .connect_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .read_timeout(std::time::Duration::from_secs(900))
            .pool_idle_timeout(std::time::Duration::from_secs(20))
            .build()?;
        let enc_user = encode_path(&account.username);
        Ok(Self {
            http,
            server_url: account.server_url.trim_end_matches('/').to_string(),
            dav_root: format!("/remote.php/dav/files/{enc_user}/"),
            username: account.username.clone(),
            password,
        })
    }

    /// Absolute URL for a DAV path (relative to the user's DAV root).
    fn url_for(&self, path: &str) -> String {
        let rel = path.trim_start_matches('/');
        let enc = rel
            .split('/')
            .map(|seg| utf8_percent_encode(seg, SEGMENT).to_string())
            .collect::<Vec<_>>()
            .join("/");
        format!("{}{}{}", self.server_url, self.dav_root, enc)
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url_for(path))
            .basic_auth(&self.username, Some(&self.password))
    }

    /// PROPFIND with Depth: 1 — list the immediate children of `path`.
    pub async fn list(&self, path: &str) -> AppResult<Vec<FileEntry>> {
        self.propfind(path, "1").await
    }

    /// Recursively search (WebDAV `SEARCH`) for entries whose name contains
    /// `query`, under `scope` (a DAV-root-relative directory; "/" searches
    /// everything). Returns up to `limit` matches, files and folders alike.
    pub async fn search(&self, query: &str, scope: &str, limit: u32) -> AppResult<Vec<FileEntry>> {
        // Scope href is relative to the DAV endpoint (/remote.php/dav), i.e.
        // `/files/<user>/<subdir>/`. Derive it from our files DAV root.
        let dav_rel = self.dav_root.strip_prefix("/remote.php/dav").unwrap_or(&self.dav_root);
        let sub = scope.trim_matches('/');
        let scope_href = if sub.is_empty() {
            dav_rel.to_string()
        } else {
            format!("{dav_rel}{}/", encode_path(sub))
        };
        let q = xml_escape(query);
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<d:searchrequest xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
  <d:basicsearch>
    <d:select><d:prop>
      <d:resourcetype/><d:getcontentlength/><d:getcontenttype/>
      <d:getlastmodified/><d:getetag/><oc:size/>
    </d:prop></d:select>
    <d:from><d:scope><d:href>{scope_href}</d:href><d:depth>infinity</d:depth></d:scope></d:from>
    <d:where><d:like><d:prop><d:displayname/></d:prop><d:literal>%{q}%</d:literal></d:like></d:where>
    <d:limit><d:nresults>{limit}</d:nresults></d:limit>
  </d:basicsearch>
</d:searchrequest>"#
        );

        let resp = self
            .http
            .request(Method::from_bytes(b"SEARCH").unwrap(), format!("{}/remote.php/dav/", self.server_url))
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/xml")
            .body(body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        // Response hrefs are the same server-absolute form as PROPFIND, so the
        // normal parser handles them; include_self=true keeps every match.
        self.parse_multistatus(&text, "/", true)
    }

    /// PROPFIND with Depth: 0 — metadata for a single resource. `None` if 404.
    pub async fn stat(&self, path: &str) -> AppResult<Option<FileEntry>> {
        match self.propfind(path, "0").await {
            Ok(mut entries) => Ok(if entries.is_empty() {
                None
            } else {
                Some(entries.remove(0))
            }),
            Err(AppError::Server { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Cheap reachability check: a minimal Depth-0 PROPFIND on the DAV root
    /// under a short *total* timeout, used by the connectivity watchdog.
    ///
    /// "Reachable" means the server answered at all — any HTTP status counts,
    /// including 401/5xx. Credential and server-side problems are a different
    /// failure than "the network is gone", and only the latter is `Offline`.
    pub async fn probe(&self) -> AppResult<()> {
        const BODY: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;
        self.request(Method::from_bytes(b"PROPFIND").unwrap(), "/")
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(BODY)
            .timeout(PROBE_TIMEOUT)
            .send()
            .await?;
        Ok(())
    }

    async fn propfind(&self, path: &str, depth: &str) -> AppResult<Vec<FileEntry>> {
        const BODY: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
  <d:prop>
    <d:resourcetype/>
    <d:getcontentlength/>
    <d:getcontenttype/>
    <d:getlastmodified/>
    <d:getetag/>
    <oc:size/>
    <oc:checksums/>
    <nc:contained-file-count/>
    <nc:contained-folder-count/>
  </d:prop>
</d:propfind>"#;

        let resp = self
            .request(Method::from_bytes(b"PROPFIND").unwrap(), path)
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(BODY)
            // A listing is a small, bounded response — unlike a transfer it has
            // no reason to take minutes, so it gets a hard cap. Without this the
            // sync walk inherits the 15-minute inactivity budget and a dropped
            // link looks like "still working" for a quarter of an hour.
            .timeout(METADATA_TIMEOUT)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        self.parse_multistatus(&text, path, depth == "0")
    }

    /// DELETE a file or (recursively) a collection.
    pub async fn delete(&self, path: &str) -> AppResult<()> {
        let resp = self.request(Method::DELETE, path).send().await?;
        ensure_ok(resp).await
    }

    /// MKCOL — create a collection (directory). Ignores "already exists".
    pub async fn mkcol(&self, path: &str) -> AppResult<()> {
        let resp = self
            .request(Method::from_bytes(b"MKCOL").unwrap(), path)
            .send()
            .await?;
        if resp.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(()); // already exists
        }
        ensure_ok(resp).await
    }

    /// Stream `path` to `dest`, calling `on_progress(transferred, total)` as data
    /// arrives (throttled to ~every 128 KiB). `total` is the server-reported size
    /// when available.
    pub async fn download_to_file(
        &self,
        path: &str,
        dest: &Path,
        mut on_progress: impl FnMut(u64, Option<u64>),
    ) -> AppResult<()> {
        // Resume: if a partial file from a previous attempt exists, ask the
        // server for the remaining bytes so a dropped/timed-out transfer picks
        // up where it left off instead of restarting from zero.
        let existing = tokio::fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
        let mut req = self.request(Method::GET, path);
        if existing > 0 {
            req = req.header("Range", format!("bytes={existing}-"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: status.as_u16(), body });
        }
        // The server honoured the resume only if it replied 206; a 200 means it
        // sent the whole file, so start the temp over.
        let resuming = existing > 0 && status.as_u16() == 206;
        let body_len = resp.content_length();
        let total = body_len.map(|c| if resuming { c + existing } else { c });

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = if resuming {
            tokio::fs::OpenOptions::new().append(true).open(dest).await?
        } else {
            tokio::fs::File::create(dest).await? // truncates any stale partial
        };
        let mut transferred = if resuming { existing } else { 0 };
        let mut last_report = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            transferred += chunk.len() as u64;
            if transferred - last_report >= 128 * 1024 {
                on_progress(transferred, total);
                last_report = transferred;
            }
        }
        file.flush().await?;
        on_progress(transferred, total.or(Some(transferred)));
        Ok(())
    }

    /// Server base URL (no trailing slash), e.g. `https://cloud.example.com`.
    pub fn server_base(&self) -> &str {
        &self.server_url
    }

    /// Authenticated OCS API call returning parsed JSON (adds `OCS-APIRequest`).
    pub async fn ocs_json(&self, path: &str) -> AppResult<serde_json::Value> {
        let resp = self
            .http
            .get(format!("{}{}", self.server_url, path))
            .basic_auth(&self.username, Some(&self.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .send()
            .await?;
        json_or_err(resp).await
    }


    /// Authenticated OCS call with a method + optional form body (POST/PUT/DELETE).
    pub async fn ocs_send(
        &self,
        method: Method,
        path: &str,
        form: &[(&str, &str)],
    ) -> AppResult<serde_json::Value> {
        let mut req = self
            .http
            .request(method, format!("{}{}", self.server_url, path))
            .basic_auth(&self.username, Some(&self.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json");
        if !form.is_empty() {
            req = req.form(form);
        }
        json_or_err(req.send().await?).await
    }

    /// Unauthenticated JSON GET (e.g. `/status.php`).
    pub async fn plain_json(&self, path: &str) -> AppResult<serde_json::Value> {
        let resp = self.http.get(format!("{}{}", self.server_url, path)).send().await?;
        json_or_err(resp).await
    }

    /// MOVE a file/collection (rename or move). `to` is a DAV-root-relative path.
    pub async fn move_to(&self, from: &str, to: &str) -> AppResult<()> {
        let dest = self.url_for(to);
        let resp = self
            .request(Method::from_bytes(b"MOVE").unwrap(), from)
            .header("Destination", dest)
            .header("Overwrite", "F")
            .send()
            .await?;
        ensure_ok(resp).await
    }

    /// MOVE that replaces the destination if it exists (`Overwrite: T`). Used to
    /// atomically publish a fully-uploaded temp file to its final path, so an
    /// interrupted upload never leaves a partial file at the real path.
    pub async fn move_replace(&self, from: &str, to: &str) -> AppResult<()> {
        let dest = self.url_for(to);
        let resp = self
            .request(Method::from_bytes(b"MOVE").unwrap(), from)
            .header("Destination", dest)
            .header("Overwrite", "T")
            .send()
            .await?;
        ensure_ok(resp).await
    }

    /// COPY a file/collection. `to` is a DAV-root-relative path.
    pub async fn copy_to(&self, from: &str, to: &str) -> AppResult<()> {
        let dest = self.url_for(to);
        let resp = self
            .request(Method::from_bytes(b"COPY").unwrap(), from)
            .header("Destination", dest)
            .header("Overwrite", "F")
            .send()
            .await?;
        ensure_ok(resp).await
    }

    /// Authenticated request against an arbitrary `/remote.php/dav/...` path
    /// (outside the user's files root — e.g. the trashbin endpoint).
    pub fn dav_request(&self, method: Method, dav_path: &str) -> reqwest::RequestBuilder {
        let enc = encode_path(dav_path.trim_start_matches('/'));
        self.http
            .request(method, format!("{}/remote.php/dav/{enc}", self.server_url))
            .basic_auth(&self.username, Some(&self.password))
    }

    /// The username this client authenticates as.
    pub fn user(&self) -> &str {
        &self.username
    }

    /// GET a file honoring an optional `Range` header — used by the `stream://`
    /// protocol for seekable media + previews. Returns the (possibly partial)
    /// bytes plus the headers the webview needs to handle range responses.
    pub async fn get_range(&self, path: &str, range: Option<&str>) -> AppResult<RangeGet> {
        let mut req = self.request(Method::GET, path);
        if let Some(r) = range {
            req = req.header("Range", r);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 206 {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: status.as_u16(), body: body.chars().take(200).collect() });
        }
        let status = status.as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let content_range = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let bytes = resp.bytes().await?.to_vec();
        Ok(RangeGet { status, content_type, content_range, bytes })
    }

    /// Stream `path` and compare it byte-by-byte with a local file, aborting
    /// the download at the first mismatch. Returns whether they are identical.
    /// `on_progress(bytes_compared)` reports every ~512 KiB.
    pub async fn compare_with_local(
        &self,
        path: &str,
        local: &Path,
        mut on_progress: impl FnMut(u64),
    ) -> AppResult<bool> {
        use tokio::io::AsyncReadExt;

        let resp = self.request(Method::GET, path).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: status.as_u16(), body });
        }

        let mut file = tokio::fs::File::open(local).await?;
        let mut local_buf = vec![0u8; 128 * 1024];
        let mut compared = 0u64;
        let mut last_report = 0u64;
        let mut stream = resp.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let mut offset = 0;
            while offset < chunk.len() {
                let n = (chunk.len() - offset).min(local_buf.len());
                // Local file ending early = mismatch, not an error.
                if file.read_exact(&mut local_buf[..n]).await.is_err() {
                    return Ok(false);
                }
                if local_buf[..n] != chunk[offset..offset + n] {
                    return Ok(false);
                }
                offset += n;
                compared += n as u64;
                if compared - last_report >= 512 * 1024 {
                    on_progress(compared);
                    last_report = compared;
                }
            }
        }
        // Identical so far — make sure the local file has no extra tail.
        let extra = file.read(&mut local_buf[..1]).await?;
        on_progress(compared);
        Ok(extra == 0)
    }

    /// Download the full contents of a file.
    pub async fn get_bytes(&self, path: &str) -> AppResult<Vec<u8>> {
        let resp = self.request(Method::GET, path).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: status.as_u16(), body });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Upload a local file as a streamed PUT, calling `on_progress(sent_bytes)`
    /// as chunks go out — so large uploads report live progress instead of
    /// jumping from 0 to done. Returns the new ETag when provided.
    pub async fn put_file_streaming(
        &self,
        path: &str,
        local: &Path,
        mut on_progress: impl FnMut(u64) + Send + 'static,
    ) -> AppResult<Option<String>> {
        use futures_util::TryStreamExt;

        let file = tokio::fs::File::open(local).await?;
        let mut sent = 0u64;
        let mut last_report = 0u64;
        let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024).inspect_ok(
            move |chunk| {
                sent += chunk.len() as u64;
                // Throttle callbacks to ~every 128 KiB.
                if sent - last_report >= 128 * 1024 {
                    on_progress(sent);
                    last_report = sent;
                }
            },
        );

        // Send the file's SHA-1 so the server stores it (`oc:checksums`) —
        // future sync verifications then hash locally instead of downloading.
        let mut req = self.request(Method::PUT, path);
        if let Some(sha1) = hash_file_sha1(local).await {
            req = req.header("OC-Checksum", format!("SHA1:{sha1}"));
        }
        let resp = req
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await?;
        let etag = resp
            .headers()
            .get("etag")
            .or_else(|| resp.headers().get("oc-etag"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        let status = resp.status();
        if status.is_success() {
            Ok(etag)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(AppError::Server {
                status: status.as_u16(),
                body: body.chars().take(500).collect(),
            })
        }
    }

    /// Upload a large local file with **chunked upload** so it survives a
    /// server request-size cap (an Apache `LimitRequestBody` / proxy limit that
    /// rejects a single big `PUT` with 413). It uploads the file in
    /// `UPLOAD_CHUNK_SIZE` pieces to a temporary `/remote.php/dav/uploads/…`
    /// session, then a single server-side `MOVE` of the magic `.file` resource
    /// assembles them onto `path` (Overwrite: T). That final MOVE is atomic, so
    /// — exactly like the single-PUT temp+move path — an interrupted upload
    /// never leaves a partial file at the real path. Returns the new ETag when
    /// the server provides one. `on_progress(sent_bytes)` fires after each chunk.
    pub async fn put_file_chunked(
        &self,
        path: &str,
        local: &Path,
        on_progress: impl FnMut(u64),
    ) -> AppResult<Option<String>> {
        self.put_file_chunked_sized(path, local, UPLOAD_CHUNK_SIZE, on_progress).await
    }

    /// Like [`put_file_chunked`] but with an explicit chunk size — used by the
    /// upload benchmark to sweep chunk sizes through the real upload path.
    pub async fn put_file_chunked_sized(
        &self,
        path: &str,
        local: &Path,
        chunk_size: usize,
        mut on_progress: impl FnMut(u64),
    ) -> AppResult<Option<String>> {
        use tokio::io::AsyncReadExt;

        let size = tokio::fs::metadata(local).await?.len();
        let dest_url = self.url_for(path);
        // A stable, per-destination session id: a retry reuses the same upload
        // dir (cleaned first) instead of leaking one session per attempt, and
        // concurrent uploads of different files never collide.
        let transfer_id = format!("cirrust-{}", short_hash(path));
        let dav_base = format!("uploads/{}/{}", self.username, transfer_id);

        // Start from a clean session: drop any leftover chunks from a failed
        // attempt (stale chunks would corrupt the assembled file), then MKCOL.
        let _ = self.dav_request(Method::DELETE, &dav_base).send().await;
        let mk = self
            .dav_request(Method::from_bytes(b"MKCOL").unwrap(), &dav_base)
            .header("Destination", &dest_url)
            .header("OC-Total-Length", size.to_string())
            .send()
            .await?;
        let st = mk.status();
        if !st.is_success() && st != StatusCode::METHOD_NOT_ALLOWED {
            let body = mk.text().await.unwrap_or_default();
            return Err(AppError::Server { status: st.as_u16(), body: body.chars().take(300).collect() });
        }

        // Upload each chunk. It's named by its zero-padded start offset so the
        // server assembles them in the right order under either a numeric or a
        // lexical sort of the chunk names.
        let mut file = tokio::fs::File::open(local).await?;
        let mut buf = vec![0u8; chunk_size];
        let mut offset: u64 = 0;
        loop {
            // read() may return short; fill the buffer up to a full chunk.
            let mut filled = 0usize;
            while filled < buf.len() {
                let n = file.read(&mut buf[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break; // clean EOF on a chunk boundary
            }
            let chunk_dav = format!("{dav_base}/{offset:016}");
            let resp = self
                .dav_request(Method::PUT, &chunk_dav)
                .body(buf[..filled].to_vec())
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    let _ = self.dav_request(Method::DELETE, &dav_base).send().await;
                    return Err(e.into());
                }
            };
            if !resp.status().is_success() {
                let s = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let _ = self.dav_request(Method::DELETE, &dav_base).send().await;
                return Err(AppError::Server { status: s.as_u16(), body: body.chars().take(300).collect() });
            }
            offset += filled as u64;
            on_progress(offset);
            if filled < buf.len() {
                break; // short read = final chunk
            }
        }

        // Assemble: MOVE the magic `.file` onto the destination in one shot.
        let mut mv = self
            .dav_request(Method::from_bytes(b"MOVE").unwrap(), &format!("{dav_base}/.file"))
            .header("Destination", &dest_url)
            .header("Overwrite", "T")
            .header("OC-Total-Length", size.to_string());
        if let Some(sha1) = hash_file_sha1(local).await {
            mv = mv.header("OC-Checksum", format!("SHA1:{sha1}"));
        }
        let resp = mv.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let _ = self.dav_request(Method::DELETE, &dav_base).send().await;
            return Err(AppError::Server { status: status.as_u16(), body: body.chars().take(300).collect() });
        }
        let etag = resp
            .headers()
            .get("etag")
            .or_else(|| resp.headers().get("oc-etag"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        Ok(etag)
    }

    /// Upload (create or overwrite) a file. Returns the server's new ETag when
    /// provided (Nextcloud sends `ETag`/`OC-ETag` on PUT).
    pub async fn put_bytes(&self, path: &str, data: Vec<u8>) -> AppResult<Option<String>> {
        let resp = self.request(Method::PUT, path).body(data).send().await?;
        let etag = resp
            .headers()
            .get("etag")
            .or_else(|| resp.headers().get("oc-etag"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        let status = resp.status();
        if status.is_success() {
            Ok(etag)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(AppError::Server { status: status.as_u16(), body: body.chars().take(500).collect() })
        }
    }

    fn parse_multistatus(
        &self,
        xml: &str,
        requested: &str,
        include_self: bool,
    ) -> AppResult<Vec<FileEntry>> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| AppError::msg(format!("bad PROPFIND xml: {e}")))?;

        let want = format!("/{}", requested.trim_matches('/'));
        let mut out = Vec::new();
        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((DAV_NS, "response")))
        {
            let href = response
                .children()
                .find(|n| n.has_tag_name((DAV_NS, "href")))
                .and_then(|n| n.text())
                .unwrap_or_default();

            // href is server-absolute and percent-encoded; convert to a path
            // relative to our DAV root.
            let decoded = percent_encoding::percent_decode_str(href)
                .decode_utf8_lossy()
                .into_owned();
            let rel = match decoded.split_once(self.dav_root.as_str()) {
                Some((_, tail)) => tail.to_string(),
                None => continue,
            };
            let rel = format!("/{}", rel.trim_start_matches('/'));

            // Skip the requested resource itself unless this is a stat (Depth 0);
            // a Depth-1 listing returns the collection plus its children.
            let rel_key = format!("/{}", rel.trim_matches('/'));
            if !include_self && rel_key == want {
                continue;
            }

            // Walk into the successful <propstat><prop>.
            let prop = response
                .descendants()
                .find(|n| n.has_tag_name((DAV_NS, "prop")));
            let is_dir = prop
                .and_then(|p| p.children().find(|n| n.has_tag_name((DAV_NS, "resourcetype"))))
                .map(|rt| rt.children().any(|n| n.has_tag_name((DAV_NS, "collection"))))
                .unwrap_or(false);

            let text_of = |ns: &str, tag: &str| -> Option<String> {
                prop.and_then(|p| p.children().find(|n| n.has_tag_name((ns, tag))))
                    .and_then(|n| n.text())
                    .map(|s| s.to_string())
            };

            // Files report getcontentlength; directories report their
            // recursive size via oc:size.
            let size = text_of(DAV_NS, "getcontentlength")
                .or_else(|| if is_dir { text_of(OC_NS, "size") } else { None })
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let mtime = text_of(DAV_NS, "getlastmodified").and_then(|s| parse_http_date(&s));
            let content_type = text_of(DAV_NS, "getcontenttype").filter(|s| !s.is_empty());
            let etag = text_of(DAV_NS, "getetag").map(|e| e.trim_matches('"').to_string());
            let file_count = text_of(NC_NS, "contained-file-count").and_then(|s| s.parse().ok());
            let dir_count = text_of(NC_NS, "contained-folder-count").and_then(|s| s.parse().ok());
            // <oc:checksums><oc:checksum>SHA1:… MD5:…</oc:checksum></oc:checksums>
            let checksums = prop
                .and_then(|p| p.descendants().find(|n| n.has_tag_name((OC_NS, "checksum"))))
                .and_then(|n| n.text())
                .map(String::from)
                .filter(|s| !s.is_empty());

            let trimmed = rel.trim_end_matches('/');
            let name = trimmed.rsplit('/').next().unwrap_or("").to_string();

            out.push(FileEntry {
                name,
                path: if is_dir && !rel.ends_with('/') {
                    format!("{rel}/")
                } else {
                    rel
                },
                is_dir,
                size,
                mtime,
                content_type,
                etag,
                file_count,
                dir_count,
                checksums,
            });
        }

        out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        Ok(out)
    }
}

const DAV_NS: &str = "DAV:";
const OC_NS: &str = "http://owncloud.org/ns";
const NC_NS: &str = "http://nextcloud.org/ns";

async fn json_or_err(resp: reqwest::Response) -> AppResult<serde_json::Value> {
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::Server { status: status.as_u16(), body: text.chars().take(300).collect() });
    }
    serde_json::from_str(&text).map_err(AppError::from)
}

async fn ensure_ok(resp: reqwest::Response) -> AppResult<()> {
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(AppError::Server { status: status.as_u16(), body: body.chars().take(500).collect() })
    }
}

/// Hashing is CPU-bound; running it inline on the async runtime only gives
/// interleaved *concurrency* on one thread. `spawn_blocking` puts each hash on
/// the blocking pool so concurrent verifications actually use multiple cores.
fn hash_file_blocking<D: sha1::Digest>(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = D::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hex::encode(hasher.finalize()))
}

/// Hash a local file with SHA-1 (lowercase hex) on a blocking thread.
pub(crate) async fn hash_file_sha1(path: &Path) -> Option<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || hash_file_blocking::<sha1::Sha1>(&path))
        .await
        .ok()
        .flatten()
}

/// Hash a local file with MD5 (lowercase hex) on a blocking thread.
pub(crate) async fn hash_file_md5(path: &Path) -> Option<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || hash_file_blocking::<md5::Md5>(&path))
        .await
        .ok()
        .flatten()
}

/// Escape the XML metacharacters so a user's search text is safe inside the
/// SEARCH request body's `<d:literal>`.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// First 16 hex chars of the SHA-1 of `s` — a compact, stable id for a string
/// (used to name a file's chunked-upload session directory).
fn short_hash(s: &str) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

/// Percent-encode a full path, preserving `/` separators.
pub(crate) fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Account;

    fn test_client() -> WebDavClient {
        let account = Account::new("https://cloud.example.com".into(), "alice".into(), crate::config::ServerKind::Nextcloud);
        WebDavClient::new(&account, "pw".into()).unwrap()
    }

    /// The dropped-VPN case: packets go into a black hole, so there is no RST
    /// and no ICMP — the socket just goes quiet. The probe must still give a
    /// verdict promptly instead of inheriting the transfer inactivity budget.
    #[tokio::test]
    async fn probe_gives_up_on_a_black_holed_server() {
        // TEST-NET-3 (RFC 5737) — reserved for documentation, never routed.
        let account = Account::new(
            "https://203.0.113.1".into(),
            "alice".into(),
            crate::config::ServerKind::Nextcloud,
        );
        let client = WebDavClient::new(&account, "pw".into()).unwrap();

        let started = std::time::Instant::now();
        assert!(client.probe().await.is_err(), "unreachable host must fail");
        assert!(
            started.elapsed() < PROBE_TIMEOUT + std::time::Duration::from_secs(5),
            "probe took {:?} — it must not fall back to the transfer timeouts",
            started.elapsed()
        );
    }

    /// A reachable server counts as online even when it rejects us. Bad or
    /// expired credentials are an *error*, not an outage, and must not paint
    /// the tray gray as if the network were down.
    ///
    ///   NC_URL=.. cargo test live_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a reachable server (NC_URL)"]
    async fn live_probe_counts_401_as_reachable() {
        let account = Account::new(
            std::env::var("NC_URL").unwrap(),
            "definitely-not-a-user".into(),
            crate::config::ServerKind::Nextcloud,
        );
        let client = WebDavClient::new(&account, "wrong".into()).unwrap();
        client.probe().await.expect("rejected but reachable");
    }

    #[test]
    fn parses_listing_and_skips_self() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/remote.php/dav/files/alice/Music/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/remote.php/dav/files/alice/Music/song.mp3</d:href>
    <d:propstat><d:prop>
      <d:resourcetype/>
      <d:getcontentlength>123</d:getcontentlength>
      <d:getcontenttype>audio/mpeg</d:getcontenttype>
      <d:getetag>&quot;abc123&quot;</d:getetag>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/remote.php/dav/files/alice/Music/sub/</d:href>
    <d:propstat><d:prop xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
      <d:resourcetype><d:collection/></d:resourcetype>
      <d:getetag>&quot;dir9&quot;</d:getetag>
      <oc:size>4096</oc:size>
      <nc:contained-file-count>7</nc:contained-file-count>
      <nc:contained-folder-count>2</nc:contained-folder-count></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
</d:multistatus>"#;

        let client = test_client();
        let entries = client.parse_multistatus(xml, "/Music", false).unwrap();
        // The collection itself is skipped; one file + one dir remain.
        assert_eq!(entries.len(), 2);

        let dir = entries.iter().find(|e| e.is_dir).unwrap();
        assert_eq!(dir.path, "/Music/sub/");
        assert_eq!(dir.name, "sub");
        assert_eq!(dir.size, 4096, "directory size from oc:size");
        assert_eq!(dir.file_count, Some(7));
        assert_eq!(dir.dir_count, Some(2));

        let file = entries.iter().find(|e| !e.is_dir).unwrap();
        assert_eq!(file.path, "/Music/song.mp3");
        assert_eq!(file.size, 123);
        assert_eq!(file.etag.as_deref(), Some("abc123"));
        assert_eq!(file.content_type.as_deref(), Some("audio/mpeg"));
    }

    /// Live end-to-end round-trip against a real Nextcloud. Ignored by default;
    /// run with:
    ///   NC_URL=.. NC_USER=.. NC_PASS=.. cargo test live_roundtrip -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_roundtrip() {
        let account = Account::new(std::env::var("NC_URL").unwrap(), std::env::var("NC_USER").unwrap(), crate::config::ServerKind::Nextcloud);
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        // 1. List root — should contain directories.
        let root = client.list("/").await.expect("list root");
        println!("root: {} entries", root.len());
        assert!(root.iter().any(|e| e.is_dir), "expected some directories");

        // 2. Fresh test collection.
        let dir = "/nc_kde_selftest";
        let file = "/nc_kde_selftest/hello.txt";
        let _ = client.delete(dir).await; // clean slate
        client.mkcol(dir).await.expect("mkcol");

        // 3. Upload — expect an ETag back.
        let etag = client
            .put_bytes(file, b"hello world".to_vec())
            .await
            .expect("put");
        println!("put etag: {etag:?}");
        assert!(etag.is_some(), "PUT should return an ETag");

        // 4. Stat.
        let st = client.stat(file).await.expect("stat").expect("file exists");
        assert_eq!(st.size, 11);
        assert!(!st.is_dir);
        assert!(st.etag.is_some());

        // 5. Download and verify bytes.
        let bytes = client.get_bytes(file).await.expect("get");
        assert_eq!(bytes, b"hello world");

        // 6. List the collection.
        let listing = client.list(dir).await.expect("list dir");
        assert_eq!(listing.len(), 1, "one child expected");
        assert_eq!(listing[0].name, "hello.txt");
        assert_eq!(listing[0].size, 11);

        // 7. Rename (MOVE) and verify the old path is gone.
        let renamed = "/nc_kde_selftest/renamed.txt";
        client.move_to(file, renamed).await.expect("move");
        assert!(client.stat(file).await.expect("stat old").is_none());
        assert!(client.stat(renamed).await.expect("stat new").is_some());

        // 8. Delete file + collection; confirm gone.
        client.delete(renamed).await.expect("delete file");
        client.delete(dir).await.expect("delete dir");
        assert!(client.stat(renamed).await.expect("stat gone").is_none());

        println!("LIVE ROUNDTRIP OK");
    }

    #[tokio::test]
    #[ignore]
    async fn live_ocs_user() {
        let account = Account::new(std::env::var("NC_URL").unwrap(), std::env::var("NC_USER").unwrap(), crate::config::ServerKind::Nextcloud);
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        let user = client.ocs_json("/ocs/v2.php/cloud/user?format=json").await.expect("ocs user");
        let quota = &user["ocs"]["data"]["quota"];
        println!("quota: {quota}");
        assert!(quota["used"].as_i64().is_some(), "quota.used present");

        let status = client.plain_json("/status.php").await.expect("status.php");
        println!("version: {}", status["versionstring"]);
        assert!(status["installed"].as_bool().unwrap_or(false));
        println!("LIVE OCS OK");
    }

    #[tokio::test]
    #[ignore]
    async fn live_range() {
        let account = Account::new(std::env::var("NC_URL").unwrap(), std::env::var("NC_USER").unwrap(), crate::config::ServerKind::Nextcloud);
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        let dir = "/nc_kde_rangetest";
        let file = "/nc_kde_rangetest/data.bin";
        let _ = client.delete(dir).await;
        client.mkcol(dir).await.unwrap();
        client.put_bytes(file, b"0123456789".to_vec()).await.unwrap();

        let rg = client.get_range(file, Some("bytes=2-5")).await.unwrap();
        println!("range status={} content_range={:?}", rg.status, rg.content_range);
        assert_eq!(rg.status, 206, "partial content");
        assert_eq!(rg.bytes, b"2345");
        assert!(rg.content_range.is_some());

        let full = client.get_range(file, None).await.unwrap();
        assert_eq!(full.bytes, b"0123456789");

        client.delete(dir).await.unwrap();
        println!("LIVE RANGE OK");
    }

    #[tokio::test]
    #[ignore]
    async fn live_share() {
        let account = Account::new(std::env::var("NC_URL").unwrap(), std::env::var("NC_USER").unwrap(), crate::config::ServerKind::Nextcloud);
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").unwrap()).unwrap();

        let dir = "/nc_kde_sharetest";
        let file = "/nc_kde_sharetest/s.txt";
        let _ = client.delete(dir).await;
        client.mkcol(dir).await.unwrap();
        client.put_bytes(file, b"hi".to_vec()).await.unwrap();

        let base = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
        let v = client
            .ocs_send(
                Method::POST,
                &format!("{base}?format=json"),
                &[("shareType", "3"), ("path", file), ("permissions", "1")],
            )
            .await
            .unwrap();
        let data = &v["ocs"]["data"];
        println!("share url: {}", data["url"]);
        assert!(data["url"].as_str().unwrap_or("").starts_with("http"), "public link url");
        let id = data["id"]
            .as_str()
            .map(String::from)
            .or_else(|| data["id"].as_i64().map(|n| n.to_string()))
            .unwrap();

        let list = client.ocs_json(&format!("{base}?format=json&path={file}")).await.unwrap();
        assert!(
            list["ocs"]["data"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
            "share listed"
        );

        client
            .ocs_send(Method::DELETE, &format!("{base}/{id}?format=json"), &[])
            .await
            .unwrap();
        client.delete(dir).await.unwrap();
        println!("LIVE SHARE OK");
    }

    #[test]
    fn url_encodes_path_segments() {
        let client = test_client();
        let url = client.url_for("/My Music/a & b.mp3");
        assert_eq!(
            url,
            "https://cloud.example.com/remote.php/dav/files/alice/My%20Music/a%20%26%20b.mp3"
        );
    }

    /// Upload-throughput benchmark: sweeps concurrency (permit budget) and the
    /// upload mode (single PUT vs chunked at several chunk sizes) across three
    /// file classes — many small, some medium, few large — and reports the
    /// fastest config per class. Mirrors the sync engine's weighted-semaphore
    /// transfer loop so the numbers reflect the real upload path.
    ///
    /// Ignored by default (needs a real server + moves real bytes). Run with:
    ///   NC_URL=.. NC_USER=.. NC_PASS=.. \
    ///     cargo test --release bench_upload_configs -- --ignored --nocapture
    ///
    /// Tune volume via env (defaults in parens): BENCH_SMALL_N (150) x
    /// BENCH_SMALL_KB (20), BENCH_MED_N (24) x BENCH_MED_MB (3),
    /// BENCH_LARGE_N (3) x BENCH_LARGE_MB (40).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn bench_upload_configs() {
        use futures_util::stream::{self, StreamExt};
        use std::sync::Arc;
        use std::time::Instant;
        use tokio::sync::Semaphore;

        fn envn(key: &str, default: u64) -> u64 {
            std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }

        let account = Account::new(
            std::env::var("NC_URL").expect("NC_URL"),
            std::env::var("NC_USER").expect("NC_USER"),
            crate::config::ServerKind::Nextcloud,
        );
        let client = WebDavClient::new(&account, std::env::var("NC_PASS").expect("NC_PASS")).unwrap();

        // Generate a local file set of `n` files of `bytes` each under `dir`.
        let workdir = std::env::temp_dir().join("cirrust-bench-src");
        let make_set = |name: &str, n: u64, bytes: u64| -> Vec<std::path::PathBuf> {
            let d = workdir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            (0..n)
                .map(|i| {
                    let p = d.join(format!("f{i:04}.bin"));
                    if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) != bytes {
                        // Cheap deterministic content; upload speed is size-bound.
                        let buf = vec![0x5au8; bytes as usize];
                        std::fs::write(&p, &buf).unwrap();
                    }
                    p
                })
                .collect()
        };

        let small = make_set("small", envn("BENCH_SMALL_N", 150), envn("BENCH_SMALL_KB", 20) * 1024);
        let medium = make_set("medium", envn("BENCH_MED_N", 24), envn("BENCH_MED_MB", 3) * 1024 * 1024);
        let large = make_set("large", envn("BENCH_LARGE_N", 3), envn("BENCH_LARGE_MB", 40) * 1024 * 1024);
        let mb_of = |files: &[std::path::PathBuf]| -> f64 {
            files.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum::<u64>() as f64
                / (1024.0 * 1024.0)
        };

        let root = "/cirrust-bench";
        let _ = client.delete(root).await;
        client.mkcol(root).await.unwrap();

        // (label, permit budget, chunk mode) — None = single streamed PUT.
        #[derive(Clone)]
        struct Cfg {
            label: &'static str,
            budget: usize,
            chunk: Option<usize>,
        }
        let mb = 1024 * 1024;

        // One measured run of `files` under a config: fresh remote dir, weighted
        // (here 1-per-file) semaphore, wall-clock around the whole batch.
        async fn run_cfg(
            client: &WebDavClient,
            root: &str,
            class: &str,
            files: &[std::path::PathBuf],
            cfg: &Cfg,
        ) -> f64 {
            let dir = format!("{root}/{class}");
            let _ = client.delete(&dir).await;
            client.mkcol(&dir).await.unwrap();
            let sem = Arc::new(Semaphore::new(cfg.budget));
            let start = Instant::now();
            let mut s = stream::iter(files.iter().cloned().enumerate().map(|(i, local)| {
                let client = client.clone();
                let sem = sem.clone();
                let remote = format!("{dir}/f{i:04}.bin");
                let chunk = cfg.chunk;
                async move {
                    let _p = sem.acquire().await.ok();
                    let r = match chunk {
                        Some(cs) => client.put_file_chunked_sized(&remote, &local, cs, |_| {}).await.map(|_| ()),
                        None => client.put_file_streaming(&remote, &local, |_| {}).await.map(|_| ()),
                    };
                    r.unwrap();
                }
            }))
            .buffer_unordered(cfg.budget * 2);
            while s.next().await.is_some() {}
            start.elapsed().as_secs_f64()
        }

        // Run every config `rounds` times, INTERLEAVED (round-robin), so slow
        // drift in a shared WLAN's bandwidth is spread across all configs
        // instead of penalising whichever happened to run during a dip. Report
        // the median per config — robust to the odd network blip.
        async fn bench_category(
            client: &WebDavClient,
            root: &str,
            class: &str,
            files: &[std::path::PathBuf],
            cfgs: &[Cfg],
            rounds: usize,
            total_mb: f64,
        ) {
            let mut samples: Vec<Vec<f64>> = vec![Vec::new(); cfgs.len()];
            for r in 0..rounds {
                for (i, c) in cfgs.iter().enumerate() {
                    let secs = run_cfg(client, root, class, files, c).await;
                    println!("  [{class}] r{}/{rounds} {:<16} {:>7.2}s ({:.2} MB/s)", r + 1, c.label, secs, total_mb / secs);
                    samples[i].push(secs);
                }
            }
            let mut rows: Vec<(String, f64)> = cfgs
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut v = samples[i].clone();
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    (c.label.to_string(), v[v.len() / 2]) // median
                })
                .collect();
            rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            println!("\n===== {class}  ({total_mb:.1} MB total, median of {rounds}) =====");
            println!("  {:<20} {:>10} {:>10}", "config", "median s", "MB/s");
            for (label, secs) in &rows {
                println!("  {:<20} {:>10.2} {:>10.2}", label, secs, total_mb / secs);
            }
            println!("  >>> FASTEST {class}: {} ({:.2}s, {:.2} MB/s)", rows[0].0, rows[0].1, total_mb / rows[0].1);
        }

        let rounds = envn("BENCH_ROUNDS", 1) as usize;

        // SMALL — latency-bound; sweep concurrency, single PUT.
        bench_category(&client, root, "small", &small, &[
            Cfg { label: "budget=4",  budget: 4,  chunk: None },
            Cfg { label: "budget=8",  budget: 8,  chunk: None },
            Cfg { label: "budget=16", budget: 16, chunk: None },
            Cfg { label: "budget=32", budget: 32, chunk: None },
        ], rounds, mb_of(&small)).await;

        // MEDIUM — mixed; sweep concurrency, single PUT.
        bench_category(&client, root, "medium", &medium, &[
            Cfg { label: "budget=2",  budget: 2,  chunk: None },
            Cfg { label: "budget=4",  budget: 4,  chunk: None },
            Cfg { label: "budget=8",  budget: 8,  chunk: None },
            Cfg { label: "budget=16", budget: 16, chunk: None },
        ], rounds, mb_of(&medium)).await;

        // LARGE — bandwidth-bound; single PUT vs chunked, low concurrency.
        bench_category(&client, root, "large", &large, &[
            Cfg { label: "b=2 single",     budget: 2, chunk: None },
            Cfg { label: "b=4 single",     budget: 4, chunk: None },
            Cfg { label: "b=2 chunk=10MB", budget: 2, chunk: Some(10 * mb) },
            Cfg { label: "b=4 chunk=10MB", budget: 4, chunk: Some(10 * mb) },
        ], rounds, mb_of(&large)).await;

        let _ = client.delete(root).await;
        println!("\nBENCH DONE");
    }
}

/// Parse an HTTP-date (RFC 1123, e.g. `Wed, 21 Oct 2015 07:28:00 GMT`) into RFC 3339.
fn parse_http_date(s: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc2822(s)
        .ok()
        .map(|dt| dt.to_rfc3339())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%a, %d %b %Y %H:%M:%S GMT")
                .ok()
                .map(|ndt| ndt.and_utc().to_rfc3339())
        })
}
