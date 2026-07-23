# Changelog

## 0.1.1 — 2026-07-23

Bug fixes for the first release.

- **Media playback in the AppImage**: the bundle shipped GStreamer's core
  libraries but none of its plugins, so WebKit found no demuxer, decoder or
  sink — video never opened and audio stayed in "preparing". The plugins are
  now bundled, and the release workflow installs them (plus `patchelf`, which
  the bundling step requires) before building.
- **Tray status before a server is reached**: a fresh install showed a green
  "up to date" tray although no account had been restored and nothing had ever
  been contacted. Both paths that published a synced state prematurely now
  report offline.

## 0.1.0 — 2026-07-23

First public release. A desktop client for Nextcloud (and compatible WebDAV
servers) for Plasma 6, built with Tauri v2, Vue 3 and Tailwind v4, plus a
native Plasma widget.

### Accounts and files

- **Auth**: Nextcloud Login Flow v2; the app password is stored in KWallet
  (Secret Service) — never on disk. Multiple accounts, switchable in place.
- **Files**: browse, search (Ctrl/Cmd+F), sort by name/size/date, upload
  (dialog + drag-and-drop), download, rename/move, duplicate, multi-select
  delete, new folder. Folder sizes and item counts with a totals status bar,
  type-ahead jump, a right-click menu at the cursor, **Open in file manager**
  (reveals the synced copy) and a sticky breadcrumb inside expanded folders.
- **Preview**: images, video, PDF and text. Image **gallery** with arrow-key
  navigation, a thumbnail filmstrip and neighbour preloading; video
  **fullscreen** (`F` / double-click).
- **Audio**: an inline player bar for tracks stored on the server, playing the
  local copy when a file is already synced and streaming it otherwise.
  **Space** toggles play/pause anywhere.
- **Trash bin**: list, restore, delete forever, empty.
- **Public links**: create (optional password and expiry), copy, revoke.

### Sync

- A custom **bidirectional WebDAV engine** with a per-folder journal, an inotify
  watcher for local edits and a periodic poll for remote ones.
- **Conflict handling** on the "conflicted copy" convention; files that are
  byte-identical on both sides are adopted rather than conflicted.
- Ignore patterns, global and per-folder pause, live progress (per file,
  overall, and transfer speed) and a recent-activity feed.
- **Chunked uploads** for large files, so servers with a request-size cap
  (`LimitRequestBody`, `post_max_size`) no longer reject them with 413.
- Downloads land in a temp file and are renamed atomically on success, so a
  failed transfer can never leave a truncated file in place.
- **Connection loss is detected while idle.** A watchdog probes the server every
  30s and reports **Offline** within that window; sync runs check reachability
  up front under a 10s cap instead of stalling inside a request, PROPFINDs are
  capped at 60s so a dead link can't hide behind the transfer budget, and idle
  pooled sockets are retired after 20s so a half-open connection isn't reused.
  Recovery is immediate: the moment the server answers again, a sync is kicked.

### Calendar and contacts

- **Calendar (CalDAV)** — two-way sync: agenda and month views, per-calendar
  colour filters, create/edit/delete events (all-day or timed, with location and
  notes) via a custom date picker, and an offline cache refreshed in the
  background on CTag changes. Right-click a day → "New event".
- **Contacts (CardDAV)** — two-way sync of address books: searchable list and
  detail view, create/edit/delete with multiple emails and phone numbers,
  organisation, title and notes.
- **Lossless editing** — an RFC 5545/6350 content-line codec preserves
  properties the app doesn't model (RRULE, VALARM, PHOTO, ADR, X-\*); writes are
  guarded by ETags (`If-Match`).

### Desktop integration

- **System tray** with a status-badged icon (synced / syncing / paused / error /
  offline), close-to-tray, and autostart (`--hidden`).
- **Plasma 6 widget** — live sync status in the panel over a session D-Bus
  service (`org.cirrust.client.Sync`), with "Sync now" and "Open".
- Panel integrations documented for **GNOME Shell** and **Cinnamon**, over that
  same D-Bus service.
- **Desktop notifications** on sync errors and new conflicts.
- **Overview**: storage quota, account and server info, server activity feed,
  start-on-login toggle, and a light / dark / **system** theme switch
  (persisted). Reduced-motion support throughout. Icons by
  [Lucide](https://lucide.dev).

### Packaging

- Releases ship the **AppImage** only. The Flatpak is built locally from a
  `.deb` that is not itself published; no `.rpm` is built.
- `packaging/install-dev-desktop.sh` for a native install (desktop entry plus a
  full icon set), and `packaging/uninstall.sh` (with `--purge`).
- A containerised build (`docker compose run --rm build`) for building without a
  local Rust/Node/WebKitGTK toolchain.

### Known limitations

- Video playback streams from a loopback `http://127.0.0.1` media server rather
  than a custom URL scheme, because WebKitGTK cannot seek a custom scheme.
- Timed calendar events are written back as **floating local time**. This
  round-trips exactly for a single-timezone user; cross-timezone precision needs
  `chrono-tz` and is a deliberate future addition.
