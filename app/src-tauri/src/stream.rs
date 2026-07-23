//! `stream://` custom URI scheme — streams a Nextcloud file into the webview
//! with HTTP Range support, so `<img>`/`<video>`/`<embed>` file previews load
//! progressively without first caching the whole file.
//!
//! The frontend builds URLs with `convertFileSrc(davPath, "stream")`.

use crate::state::AppState;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager};

pub async fn serve(app: AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    match serve_inner(&app, &request).await {
        Ok(resp) => resp,
        Err(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(e.into_bytes())
            .unwrap(),
    }
}

async fn serve_inner(
    app: &AppHandle,
    request: &Request<Vec<u8>>,
) -> Result<Response<Vec<u8>>, String> {
    // The DAV path is the (percent-encoded) URI path; decode it back.
    let raw = request.uri().path().trim_start_matches('/');
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let path = format!("/{}", decoded.trim_start_matches('/'));

    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let start: u64 = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|spec| spec.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let is_local = tokio::fs::metadata(&path).await.map(|m| m.is_file()).unwrap_or(false);
    log::info!("stream: path={path:?} range={range:?} local={is_local}");

    // Local file (a synced media file or an app-cache copy — the frontend passes
    // the absolute local path when one exists): serve straight from disk with
    // real Range support. This is what makes audio/video actually play, and it's
    // instant + seekable since we never round-trip to the server.
    if is_local {
        let r = serve_local(&path, range.as_deref()).await;
        if let Err(ref e) = r {
            log::warn!("stream: serve_local failed for {path:?}: {e}");
        }
        return r;
    }

    let client = app.state::<AppState>().client().await.map_err(|e| e.to_string())?;

    // The webview's media source for a custom URI scheme issues ONE request per
    // load and never follows up with more ranges — so a partial response leaves
    // `<video>`/`<embed>` stuck. Serve everything from the requested offset to
    // the end in that single response: the initial probe (`bytes=0-…`) gets the
    // full file (200); a seek (`bytes=START-…`) gets START→end (206). We still
    // advertise Accept-Ranges so seeking works.
    let fetch_range = if start > 0 { Some(format!("bytes={start}-")) } else { None };

    let rg = client
        .get_range(&path, fetch_range.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    let mut builder = Response::builder()
        .status(StatusCode::from_u16(rg.status).unwrap_or(StatusCode::OK))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, rg.bytes.len().to_string());
    if let Some(ct) = rg.content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    if let Some(cr) = rg.content_range {
        builder = builder.header(header::CONTENT_RANGE, cr);
    }
    builder.body(rg.bytes).map_err(|e| e.to_string())
}

/// Serve a local file (image/PDF preview), honoring an HTTP `Range` request.
/// `<img>`/`<embed>` do a single one-shot load, so returning the exact requested
/// slice (or the whole file when there's no `Range` header) is enough.
/// `Content-Type` comes from the extension; reading off disk is instant.
///
/// Note: `<video>`/`<audio>` do NOT use this — WebKitGTK's custom-scheme media
/// loader can't seek (MediaError code 4), so playback goes through the loopback
/// HTTP server in `mediahttp.rs` instead.
async fn serve_local(path: &str, range: Option<&str>) -> Result<Response<Vec<u8>>, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let size = tokio::fs::metadata(path).await.map_err(|e| e.to_string())?.len();
    let last = size.saturating_sub(1);

    // Parse "bytes=start-end" (end optional). Absent header → full file.
    let parsed = range.and_then(|r| r.strip_prefix("bytes=")).map(|spec| {
        let mut parts = spec.splitn(2, '-');
        let start = parts.next().unwrap_or("").trim().parse::<u64>().unwrap_or(0);
        let end = match parts.next().map(str::trim) {
            Some(e) if !e.is_empty() => e.parse::<u64>().unwrap_or(last),
            _ => last,
        };
        (start.min(last), end.min(last))
    });

    let (start, end, is_range) = match parsed {
        Some((s, e)) => (s, e.max(s), true),
        None => (0, last, false),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };

    let mut file = tokio::fs::File::open(path).await.map_err(|e| e.to_string())?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| e.to_string())?;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;

    let status = if is_range { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, mime_for(path))
        .header(header::CONTENT_LENGTH, bytes.len().to_string());
    if is_range {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{size}"),
        );
    }
    builder.body(bytes).map_err(|e| e.to_string())
}

/// Best-effort MIME type from a file extension for the media the previewer and
/// audio player handle. Falls back to `application/octet-stream`.
fn mime_for(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        // Audio
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        // Video
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        // Images
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}
