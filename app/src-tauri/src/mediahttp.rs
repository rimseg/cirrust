//! Minimal `127.0.0.1` HTTP server for media playback.
//!
//! WebKitGTK's custom URI scheme handler (`stream://`) can't drive a `<video>`
//! element: playback dies with `MediaError` code 4 and no duration, because that
//! code path doesn't support the ranged/seek requests GStreamer's demuxer needs.
//! A **real `http://` origin** goes through GStreamer's `souphttpsrc`, which
//! handles Range natively — so the same file that fails over `stream://` (or a
//! Blob URL, which never resolves its duration) plays correctly, seekable, with
//! a proper timeline, when served over HTTP.
//!
//! We therefore serve already-on-disk media (synced copies or the media cache)
//! over loopback. Access is guarded by a random per-session token embedded in
//! the URL path and by an allow-list of roots (sync folders + the media cache),
//! and the listener is bound to `127.0.0.1` only.

use crate::config::AppConfig;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct MediaServer {
    app: AppHandle,
    port: u16,
    token: String,
}

impl MediaServer {
    /// Bind a loopback listener on a random port and spawn the accept loop.
    pub async fn start(app: AppHandle) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        log::info!("media http server on 127.0.0.1:{port}");

        let app_bg = app.clone();
        let tok = token.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let app = app_bg.clone();
                        let tok = tok.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle(stream, &app, &tok).await {
                                log::debug!("media http request failed: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        log::warn!("media http accept error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(MediaServer { app, port, token })
    }

    /// Build the playback URL for an absolute local file path.
    pub fn url_for(&self, local_path: &str) -> String {
        let enc = percent_encoding::utf8_percent_encode(
            local_path,
            percent_encoding::NON_ALPHANUMERIC,
        );
        format!("http://127.0.0.1:{}/{}/{}", self.port, self.token, enc)
    }

    /// Whether `path` is inside a root we're willing to serve (a synced folder or
    /// the media cache). Prevents the token, if it ever leaked, from reading
    /// arbitrary files off disk.
    fn is_allowed(&self, path: &Path) -> bool {
        let canon = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Ok(cfg) = AppConfig::load(&self.app) {
            roots.extend(cfg.sync_folders.iter().map(|f| PathBuf::from(&f.local_path)));
        }
        if let Ok(mut cache) = self.app.path().app_cache_dir() {
            cache.push("media");
            roots.push(cache);
        }
        roots.iter().any(|r| r.canonicalize().map(|r| canon.starts_with(r)).unwrap_or(false))
    }
}

/// 32 hex chars from the OS CSPRNG — an unguessable per-session path prefix.
fn random_token() -> String {
    let mut buf = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        // Extremely unlikely on Linux; fall back to a process-derived value so we
        // still start (loopback-only, so this is defence in depth either way).
        let pid = std::process::id().to_le_bytes();
        buf[..4].copy_from_slice(&pid);
    }
    hex::encode(buf)
}

async fn handle(mut stream: TcpStream, app: &AppHandle, token: &str) -> std::io::Result<()> {
    // Read request head (headers are tiny; cap so a bad client can't OOM us).
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    let range = lines
        .find_map(|l| l.strip_prefix("Range:").or_else(|| l.strip_prefix("range:")))
        .map(|v| v.trim().to_string());

    // Path is /<token>/<percent-encoded absolute path>.
    let rest = target.trim_start_matches('/');
    let (got_token, enc_path) = rest.split_once('/').unwrap_or(("", ""));
    if got_token != token {
        return write_simple(&mut stream, 403, "Forbidden").await;
    }
    let path = percent_encoding::percent_decode_str(enc_path).decode_utf8_lossy().into_owned();
    let p = Path::new(&path);

    let server = app.state::<MediaServer>();
    if !p.is_file() || !server.is_allowed(p) {
        return write_simple(&mut stream, 404, "Not Found").await;
    }

    serve_file(&mut stream, &path, range.as_deref(), method == "HEAD").await
}

async fn serve_file(
    stream: &mut TcpStream,
    path: &str,
    range: Option<&str>,
    head_only: bool,
) -> std::io::Result<()> {
    let size = tokio::fs::metadata(path).await?.len();
    let last = size.saturating_sub(1);

    // Parse "bytes=start-end" (end optional).
    let parsed = range.and_then(|r| r.strip_prefix("bytes=")).map(|spec| {
        let mut it = spec.splitn(2, '-');
        let start = it.next().unwrap_or("").trim().parse::<u64>().unwrap_or(0);
        let end = match it.next().map(str::trim) {
            Some(e) if !e.is_empty() => e.parse::<u64>().unwrap_or(last),
            _ => last,
        };
        (start.min(last), end.min(last).max(start.min(last)))
    });
    let (start, end, is_range) = match parsed {
        Some((s, e)) => (s, e, true),
        None => (0, last, false),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };

    let status = if is_range { "206 Partial Content" } else { "200 OK" };
    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        ct = mime_for(path),
    );
    if is_range {
        header.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;
    if head_only || len == 0 {
        return stream.flush().await;
    }

    // Stream the body in chunks so a large file never sits fully in memory.
    let mut file = tokio::fs::File::open(path).await?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let mut remaining = len;
    let mut chunk = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(chunk.len() as u64) as usize;
        let n = file.read(&mut chunk[..want]).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&chunk[..n]).await?;
        remaining -= n as u64;
    }
    stream.flush().await
}

async fn write_simple(stream: &mut TcpStream, code: u16, msg: &str) -> std::io::Result<()> {
    let body = msg.as_bytes();
    let resp = format!(
        "HTTP/1.1 {code} {msg}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Best-effort MIME type from a file extension (mirrors `stream::mime_for`).
fn mime_for(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        _ => "application/octet-stream",
    }
}
