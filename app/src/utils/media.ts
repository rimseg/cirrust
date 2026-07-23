// Resolving a viewable/playable source for a remote file.
//
// Everything here goes through the `stream://` protocol — a local copy when the
// file is synced, else the server. For a local file the Rust handler serves
// straight off disk with real HTTP Range support (206 + Content-Range), which
// is what lets a `<video>` element seek, learn its own duration, and play to the
// end instead of looping the first fragment forever.
//
// (Audio does NOT use this — the bottom PlayerBar decodes whole tracks through
// the Web Audio API in `stores/player.ts` for clean, gapless playback.)

import { convertFileSrc } from "@tauri-apps/api/core";
import { media } from "../api";
import type { FileEntry } from "../api/types";

/** URL for an image/pdf: local file when synced, else the remote file. */
export async function imageSrc(entry: FileEntry): Promise<string> {
  const local = await media.localPath(entry.path).catch(() => null);
  return convertFileSrc(local ?? entry.path, "stream");
}

/**
 * URL for a `<video>` preview: a loopback `http://127.0.0.1` URL served by the
 * backend media server. WebKitGTK will only seek a real HTTP origin — both the
 * `stream://` custom scheme (MediaError code 4) and Blob URLs (duration never
 * resolves → no timeline, first fragment loops) fail. The file is played from
 * its synced local copy when present, else downloaded into the cache first.
 */
export async function playableSrc(entry: FileEntry): Promise<string> {
  return media.httpUrl(entry.path, entry.etag);
}
