# Changelog

## Unreleased

- **Pausing now actually stops a running sync.** The pause flag used to be read
  only once, at the start of a round — pausing (or disabling a folder) while a
  sync was running let every queued transfer finish, potentially gigabytes
  later, and the Pause button visibly snapped back to unpaused while a run was
  in flight. Cancellation is now checked throughout the engine: the scan stops,
  no new transfers start, in-flight transfers are aborted (partially downloaded
  temps are kept and resumed later), and pending work is journaled so the next
  run continues exactly where the paused one stopped. The paused state is
  reported immediately, per-folder pause cancels that folder's in-flight sync
  too, and pausing no longer blanks the "last sync" timestamp.
- **Sync is two-way only.** One-way (upload-only / download-only) modes were
  added during this cycle and removed again before release: partial modes
  multiply the states in which a reconciliation mistake destroys data. The
  full two-way decision matrix is now locked down by an exhaustive unit test.
- **Data-safety guards in the sync engine.**
  - *Mass-deletion guard:* a deletion sweep that would remove ≥10 entries and
    at least half of one side is treated as state loss (unmounted disk,
    renamed or emptied local folder, hollow server answer) — the deletions are
    refused and the next run **restores** the surviving files to the missing
    side instead. When in doubt the engine re-transfers; it never mass-deletes.
  - *No silent local-scan failures:* an unreadable local folder or file now
    aborts that folder's run with an error instead of being misread as "the
    user deleted everything".
  - *New pairs can't absorb existing server data:* adding a folder whose
    remote name is already taken by a populated server folder now syncs to
    `"<name> 2"` instead of merging into (and potentially later deleting)
    unrelated files — like a file manager resolving a name collision. Pulling
    an existing server folder down via "Folders on your server" pairs with it
    directly, as intended there.
- **Unified "Folders" list.** Overview now shows a single list: the cloud's
  folder tree (navigable) with each folder's sync state overlaid. Folders
  synced to this computer show their destination, live status and
  pause/remove controls inline; unsynced ones get a one-click **Sync** that
  downloads them into `~/Nextcloud/<name>` — the missing "new client" flow.
  Subfolders of an already-synced pair are marked "synced as part of …"
  (which also prevents accidentally double-pairing them), and synced pairs
  not visible at the current browse level are appended so the list is always
  complete. The "Sync a new folder" form remains for local-first pairs; its
  local path is free text and is created on first sync.
- Testing: live suite extended with download-only / upload-only scenario tests
  and pause-cancellation tests (nothing transfers after a cancel; a pending
  deletion survives a pause instead of resurrecting the file). Also de-flaked
  the suite: Nextcloud ETags and local mtimes are second-granular, so a test
  mutation in the same second as the previous sync was genuinely undetectable —
  the harness now steps past the second boundary after every sync.

## 0.1.7 — 2026-07-24

- **Folder deletion now propagates when the folder held files.** Deleting a
  folder that contained files used to remove the files on the other side but
  leave an empty ghost folder that kept coming back — a directory's ETag changes
  whenever its contents do, so the folder looked "modified" and was re-created
  instead of removed. The sync engine now decides directory deletion by whether
  it knew the folder, not by its ETag, and only removes a remote folder once it
  is actually empty — so a file added on the far side after you deleted the
  folder is preserved rather than swept away.
- Testing: a live sync/PIM test suite (`packaging/live-tests.sh`) that runs the
  full reconcile matrix — file create/modify/delete, every conflict path,
  directory deletion (empty and non-empty), nested trees, ignore patterns — plus
  CalDAV/CardDAV round-trips with ETag-guarded writes, against a throwaway
  Nextcloud. Run green before each release.

## 0.1.6 — 2026-07-24

- **Self-updating AppImage**: the self-install from 0.1.5 was first-run only —
  once a copy existed in `~/.local/bin`, a newer AppImage neither prompted nor
  replaced it. Now the installed copy records its version, and a newer AppImage
  offers — via a one-time dialog — to update it. Declining is remembered per
  version, so it only asks again for the next release. An install predating this
  is offered the update once. `--install` still overwrites unconditionally.
  (Updating via the prompt needs the installed copy not to be running, or the
  single-instance guard defers the new launch before the dialog appears;
  `--install` is unaffected.)

## 0.1.5 — 2026-07-24

- **Self-installing AppImage**: on the first run from an AppImage, Cirrust now
  offers — via a one-time dialog — to add itself to your applications menu. If you
  accept, it copies itself to `~/.local/bin/cirrust` and registers a desktop
  entry and icons, so it launches from the menu, the tray and (via the in-app
  "Start on login" toggle) at login, all pointing at a stable path rather than the
  AppImage's temporary mount. Declining is remembered and not asked again. New
  `--install` / `--uninstall` flags do the same non-interactively; `--uninstall`
  also removes the autostart entry.
- **Login screen** now shows the Cirrust brand mark instead of a generic cloud
  glyph, matching the app icon and favicon.

## 0.1.4 — 2026-07-24

A GNOME-integration and documentation release. The desktop app itself is
unchanged from 0.1.3 — the AppImage is functionally identical. This exists so
that installing from a release no longer hands GNOME users a broken indicator.

- **GNOME Shell 50**: the extension declared `shell-version` up to 49, and Shell
  refuses to load anything that does not list the running version — so on GNOME
  50 the indicator did not appear at all. Now declared through 50 and verified on
  a real Shell 50.3 (`State: ACTIVE`, no JS exceptions).
- **GNOME indicator icons**: `adwaita-icon-theme` 50 dropped
  `emblem-default-symbolic` and `emblem-synchronizing-symbolic`, and
  `adwaita-icon-theme-legacy` does not supply them either, so the panel icon
  rendered blank in the *idle* and *syncing* states — almost always. Replaced
  with `object-select-symbolic` and `view-refresh-symbolic`.
- Renamed the extension's two leftover pre-rename classes to
  `CirrustIndicator` / `CirrustExtension`. No behaviour change.
- **Documentation corrections**, each checked against the code: dropped a claimed
  `live_index` test and "music-tag indexing" that do not exist; corrected three
  live-test names; "the sidebar shows a live speed readout" → the top bar (there
  is no sidebar); removed "thumbnails" from the `webdav.rs` description. Also
  dropped two roadmap phase numbers that referred to a plan not in this repo.
- **Corrected the folder-deletion limit**, which was documented backwards.
  Measured against a live server: deleting an *empty* folder propagates in both
  directions; the actual limit is deleting a folder that still held files — its
  files are removed on the other side, but the now-empty folder is left behind
  and re-created. Deleting the leftover once finishes the job.
- New README image: the Overview split by a wave into light and dark, with four
  further views peeking out from behind it. All screenshots were retaken against
  a throwaway demo server — the previous one exposed a real account name, an
  internal hostname and local paths.

## 0.1.3 — 2026-07-23

- **First-play freeze**: the first audio track or video after launch stalled the
  UI for several seconds while GStreamer built its plugin registry on first use
  (a bigger cost now that the AppImage bundles its own codecs). The decoder is
  now primed in the background at startup, so that one-time scan no longer lands
  on the first play or preview.
- Dropped the unused `@tauri-apps/plugin-notification` JS package (notifications
  are sent from the Rust backend), and reworded the README to describe Cirrust as
  a cross-distro Linux client rather than KDE/Manjaro-specific.

## 0.1.2 — 2026-07-23

- **Blank white AppImage window**: the bundled WebKitGTK was inherited from an
  Ubuntu 22.04 build base, too old to initialise EGL on very new Mesa — the web
  process aborted before painting and the window came up blank. The AppImage
  (CI and Docker) now builds on a newer base whose WebKit drives current GPU
  stacks. This raises the glibc floor: the AppImage needs a reasonably recent
  host (glibc 2.41+); on older systems, build from source or use the Flatpak.
- **Flatpak media playback**: declared the `ffmpeg-full` codec extension the
  GNOME runtime omits, so H.264/AAC/MP3 play there too, and moved to the GNOME
  48 runtime.

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
