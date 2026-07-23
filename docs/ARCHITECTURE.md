# Architecture notes

Cirrust is a **Tauri v2** desktop app: a **Rust** backend and a **Vue 3 + TypeScript
+ Pinia + Tailwind v4** frontend, plus panel widgets for KDE/GNOME/Cinnamon that
talk to the backend over session D-Bus. It targets Linux / WebKitGTK.

```
┌──────────────────────────────────────────────┐        ┌──────────────────┐
│  app/  (Tauri v2 desktop app)                 │        │ widget/, …       │
│                                               │ D-Bus  │ Plasma/GNOME/    │
│  ┌────────────┐  invoke/    ┌──────────────┐  │◄───────┤ Cinnamon panels  │
│  │ Vue 3 + TS │ ─────────►  │ Rust backend │  │        │ sync status +    │
│  │ Tailwind v4│ ◄─────────  │ auth·webdav  │  │        │ "sync now"       │
│  └────────────┘  events     │ sync·pim·... │  │        └──────────────────┘
│   views/stores              └──────┬───────┘  │
└─────────────────────────────────────┼─────────┘
                                       │ WebDAV / CalDAV / CardDAV / OCS
                                       ▼
                              Nextcloud / ownCloud
```

- **Frontend** (`app/src`): Vue 3, Pinia stores, Vue Router (hash history), Tailwind
  v4. Every backend call goes through the typed layer in `app/src/api/index.ts`
  (types in `api/types.ts`); components never call `invoke` directly.
- **Backend** (`app/src-tauri/src`): Rust. `camelCase` is used on both sides of the
  IPC boundary (serde `rename_all`). All fallible commands return
  `AppResult<T>` (`Result<T, AppError>`); `AppError` is `Serialize`, so a failure
  surfaces as a rejected JS promise carrying `{ kind, message }`.
- The app keeps running in the **system tray** so background sync continues after
  the window is closed.

## Command surface (Rust ↔ Vue)

Registered in `lib.rs` `tauri::generate_handler![…]`; wrapped by `api/index.ts`.
Args below omit the injected `AppHandle` / `State` handles. **48 commands.**

### auth — `auth.rs`
| Command | Args | Returns |
|---|---|---|
| `auth_start_login` | `serverUrl` | `LoginFlowInit` |
| `auth_poll_login` | `pollEndpoint`, `pollToken` | `Account \| null` |
| `auth_add_manual` | `serverUrl`, `username`, `password`, `kind` | `Account` |
| `auth_list_accounts` | — | `Account[]` |
| `auth_active_account` | — | `Account \| null` |
| `auth_set_active_account` | `accountId` | — |
| `auth_remove_account` | `accountId` | — |

### files — `files.rs`
| Command | Args | Returns |
|---|---|---|
| `files_list` | `path` | `FileEntry[]` |
| `files_search` | `query`, `scope` | `FileEntry[]` |
| `files_delete` | `path` | — |
| `files_download` | `path`, `localPath` | — |
| `files_upload` | `remoteDir`, `localPaths` | — |
| `files_mkdir` | `path` | — |
| `files_move` | `from`, `to` | — |
| `files_copy` | `from`, `to` | — |
| `files_read_text` | `path` | `string` |

### media — `media.rs`
| Command | Args | Returns |
|---|---|---|
| `media_local_path` | `path` | `string \| null` (synced **file**) |
| `media_reveal_path` | `path` | `string \| null` (synced file **or dir**) |
| `media_cache` | `path`, `etag?` | `string` (local path; downloads if unsynced) |
| `media_http_url` | `path`, `etag?` | `string` (loopback `http://` URL) |
| `media_bytes` | `path` | `ArrayBuffer` |

### trash — `trash.rs`
`trash_list` → `TrashEntry[]` · `trash_restore(trashId)` · `trash_delete(trashId)` · `trash_empty`

### sync — `sync/mod.rs`
| Command | Args | Returns |
|---|---|---|
| `sync_list_folders` | — | `SyncFolder[]` |
| `sync_folder_stats` | — | `FolderStat[]` |
| `sync_add_folder` | `localPath`, `remotePath`, `accountId?` | `SyncFolder` |
| `sync_remove_folder` | `id` | — |
| `sync_status` | — | `SyncStatus` |
| `sync_progress` | — | `SyncProgress` |
| `sync_activity` | — | `SyncActivity[]` |
| `sync_now` | — | — |
| `sync_set_paused` | `paused` | — |
| `sync_set_folder_enabled` | `id`, `enabled` | — |
| `sync_settings` | — | `SyncSettings` |
| `sync_set_ignore_patterns` | `patterns` | — |
| `sync_conflicts` | — | `Conflict[]` |
| `sync_resolve_conflict` | `localPath`, `keep` (`"local"\|"remote"`) | — |
| `sync_dismiss_identical_conflicts` | — | `number` (count) |

### dashboard — `dashboard.rs`
`account_info` → `AccountInfo` · `account_activity` → `ActivityItem[]`

### sharing — `sharing.rs`
`shares_list(path?)` → `Share[]` · `share_create(path, password?, expireDate?)` → `Share` · `share_delete(id)`

### caldav — `pim/caldav.rs`
`caldav_calendars` / `caldav_refresh` → `CalendarInfo[]` · `caldav_events(calendarIds?)` → `CalEvent[]` · `caldav_save_event({calendarId, event, href?, etag?})` → `CalEvent` · `caldav_delete_event(calendarId, href, etag?)`

### carddav — `pim/carddav.rs`
`carddav_addressbooks` / `carddav_refresh` → `AddressBookInfo[]` · `carddav_contacts(addressbookIds?)` → `Contact[]` · `carddav_save_contact({addressbookId, contact, href?, etag?})` → `Contact` · `carddav_delete_contact(addressbookId, href, etag?)`

## Backend modules (`app/src-tauri/src`)

- `main.rs` — binary entry; calls `app_lib::run()`.
- `lib.rs` — Tauri builder: registers the `stream://` scheme, plugins, the invoke
  handler, the tray, close-to-tray, the single-instance D-Bus guard, the media
  HTTP server, and the sync engine. Sets WebKit/GTK env workarounds
  (`GTK_CSD=0`, `GDK_BACKEND=x11` on Wayland, `WEBKIT_DISABLE_DMABUF_RENDERER=1`).
- `auth.rs` — Login Flow v2 + manual app-password login (Nextcloud/ownCloud); OS
  keyring (KWallet/Secret Service) storage; session restore.
- `config.rs` — persistent non-secret JSON config (accounts, sync folders, pause,
  ignore patterns, active account); `ServerKind`.
- `state.rs` — `AppState`: one authenticated `WebDavClient` per account + the
  active (browsed) account.
- `error.rs` — `AppError` / `AppResult`, serialized across IPC.
- `webdav.rs` — the WebDAV client for `/remote.php/dav` (PROPFIND, GET/PUT/DELETE,
  MKCOL, MOVE/COPY, SEARCH, ranged GET, OCS helpers, `dav_request` for arbitrary
  DAV paths). Shared by files, sync, PIM, sharing, dashboard.
- `files.rs` — file-browser commands over the WebDAV client.
- `media.rs` — resolve synced local paths (file / reveal file-or-dir), cache a
  download, raw bytes, and the loopback media URL.
- `stream.rs` — the **`stream://` scheme handler**: streams a Nextcloud file (or
  local synced copy) into the webview with HTTP **Range** for `<img>`/`<embed>`
  previews (images/PDF). Video/audio do **not** use this — see below.
- `mediahttp.rs` — a **loopback HTTP server** (`127.0.0.1`, random per-session
  token) that serves on-disk media so WebKitGTK/GStreamer can seek `<video>`.
- `trash.rs` / `sharing.rs` / `dashboard.rs` — OCS trashbin, shares, quota/activity.
- `tray_badge.rs` — composites a status-coloured badge onto the tray icon
  (green/blue/orange/red/gray), repainting only on state change.
- `pim/` — **CalDAV + CardDAV**:
  - `dav.rs` — generic DAV verbs (PROPFIND/REPORT/PUT/DELETE/GET) + a
    namespace-agnostic `multistatus` parser + `href_to_dav_path`.
  - `contentline.rs` — lossless RFC 5545/6350 content-line codec (params, folding,
    TEXT escaping) shared by ical/vcard.
  - `ical.rs` / `vcard.rs` — VEVENT / vCard 3.0 ⇄ display models; **lossless**
    patch (preserve RRULE/VALARM/PHOTO/ADR/X-* …).
  - `caldav.rs` / `carddav.rs` — discovery + CTag-gated refresh + two-way
    save/delete + per-account cache.
  - `store.rs` — JSON cache under `<app_data_dir>/pim/<account_id>/`.
- `sync/` — folder sync:
  - `engine.rs` — three-way (remote / local / journal) bidirectional
    reconciliation for one folder pair; conflict handling.
  - `journal.rs` — per-folder JSON journal (the three-way merge base).
  - `progress.rs` — live progress + activity feed, published on a `watch` channel
    and the `sync://progress` event.
  - `mod.rs` — `SyncManager` (startup + timer + trigger + fs-watcher) and the
    `sync_*` commands.
  - `dbus.rs` — the session D-Bus service `org.cirrust.client.Daemon` (`/Sync`,
    interface `org.cirrust.client.Sync`) — feeds the panel widgets **and** doubles
    as the single-instance guard.

**Background services:** the media HTTP server, the sync manager, the D-Bus
daemon/instance-guard, and the tray.

## Frontend (`app/src`)

- **views/**: `LoginView`, `OverviewView` (dashboard + synced-folder management +
  theme switch), `FilesView` (tree browser), `CalendarView` (agenda + month),
  `ContactsView`, `TrashView`.
- **components/**: `TopBar` (nav + account switcher), `PlayerBar` (bottom audio
  player; global Space play/pause), `FilePreview` (image gallery + filmstrip,
  video, PDF, text), `ShareDialog`, `DatePicker` (custom teleported popover).
- **stores/**: `auth`, `player` (Web Audio decoding for gapless playback), `sync`
  (live from `sync://` events).
- **utils/**: `format` (size/speed/date), `media` (`imageSrc`/`playableSrc`),
  `download` (`downloadOrReveal`), `theme` (light/dark/system), `date` (shared
  calendar/date helpers), plus the `useContextMenu` composable + `ContextMenu`
  component used by the right-click menus.
- **router**: `/overview`, `/files/:path(.*)?`, `/calendar`, `/contacts`, `/trash`,
  `/login`; `/` and `/sync` redirect to `/overview`. A `beforeEach` guard bounces
  to `/login` when no account is active.

## Notable subsystems

### Media playback (why the loopback HTTP server exists)
On WebKitGTK a `<video>` cannot seek from the `stream://` custom scheme
(`MediaError` code 4, no duration), and Blob URLs never resolve a duration. Only a
real `http://` origin routes through GStreamer's `souphttpsrc` with native Range.
So `mediahttp::MediaServer` serves already-on-disk media (the synced copy or the
media cache) over `127.0.0.1` behind a random per-session token; the frontend's
`playableSrc()` returns `media.httpUrl(...)`. Images still use the Range-capable
`stream://` scheme. Two more WebKitGTK workarounds: `WEBKIT_DISABLE_DMABUF_RENDERER=1`
(else decoded frames are black) and **window**-level fullscreen instead of native
element fullscreen (native path is also black).

### CalDAV / CardDAV (two-way, lossless)
Layered on the authenticated `WebDavClient` via `pim/dav.rs`, against
`calendars/{user}/` and `addressbooks/users/{user}/` (note the extra `users/` for
CardDAV). Per-collection **CTag** decides whether to refetch; data is cached per
account on disk for instant/offline open. Edits are lossless because
`contentline.rs` round-trips the shared RFC 5545/6350 grammar — `ical.rs`/`vcard.rs`
patch existing objects in place, preserving properties the UI doesn't model. Writes
are guarded with `If-Match`/`If-None-Match` ETags. **Timezone limitation (v1):** no
bundled IANA tz DB — all-day events use `VALUE=DATE`; timed events are wall-clock
(UTC `Z` is converted to machine-local for display, written back floating).

### Sync engine
A journaled, **bidirectional** engine. A three-way diff (remote ETag vs. local
size/mtime vs. the per-folder journal) classifies each path as upload / download /
delete-local / delete-remote / conflict; deletions propagate both ways.
Byte-identical files on both sides are adopted silently; genuine divergence yields a
`name (conflicted copy DATE).ext` while taking the server's version (matching the
official client). Runs on startup, every ~5 min, on demand (`sync_now` / tray /
widget), and reacts to local changes via an inotify watcher. Live `SyncStatus` /
`SyncProgress` feed the UI (`sync://` events) and the panel widgets (D-Bus).

### Theme
`utils/theme.ts` + `styles.css`: light / dark / system, persisted in `localStorage`
(`cirrust-theme`). "system" removes the override and follows
`@media (prefers-color-scheme)`; an explicit choice stamps `data-theme` on `<html>`,
which wins over the media query. `initTheme()` runs before mount to avoid a flash.

### Desktop integration
Session D-Bus service `org.cirrust.client.Daemon` (`/Sync`: `Status()`, `SyncNow()`,
`Open()`). Consumed by the **Plasma 6 applet** (`widget/`), **GNOME Shell extension**
and **Cinnamon applet** (`integrations/`); everywhere else the status-badged
StatusNotifier tray covers it. Try it while running:

```bash
gdbus call --session --dest org.cirrust.client.Daemon \
  --object-path /Sync --method org.cirrust.client.Sync.Status
```

## Packaging
`tauri.conf.json` `bundle.targets = ["appimage", "deb"]`. Only the **AppImage** is
shipped (CI builds it with `--bundles appimage`); the `.deb` exists **solely** as
the input the Flatpak manifest wraps (`packaging/flatpak/`). `.rpm` is not built.
"Native" install = `npm run tauri build` + copy the binary + `install-dev-desktop.sh`.
See the README for details.

## Known duplication / refactor backlog

**Consolidated** in this pass: `fetch_etag` (→ `WebDavClient::dav_fetch_etag`),
`now_utc` (→ `pim/mod.rs`), the save-copy download closure (→ `utils/download.ts`),
and the calendar/date helpers (→ `utils/date.ts`).

**Backlog** — genuine parallel logic left for a future pass (deferred here because
it touches interactive UI or the write path and needs manual/round-trip testing):

- **Right-click menus** — the `{open,x,y,item}` state + fixed-position menu chrome
  repeats in `FilesView`, `ContactsView`, `TrashView`, `CalendarView`. Extract a
  `useContextMenu<T>()` composable + a `<ContextMenu>` component (use FilesView's
  viewport clamping + cursor/under-button open as the canonical behavior).
- **`caldav.rs` ↔ `carddav.rs`** share the same discover/refresh/upsert/save/delete
  algorithm with `Info/Item/content-type` swapped — a generic `DavCollection` trait
  would collapse ~150 lines but touches the write path.
- **Editor-modal chrome** is near-identical in `CalendarView` and `ContactsView`
  (and `ShareDialog`) — a `<Modal>` wrapper with header/body/footer slots.
- **`useAutoRefresh`** — Contacts/Calendar share the load → background-refresh →
  5-min-timer lifecycle.
- **WebDAV error-for-status** boilerplate repeats ~10× in `webdav.rs` (+ `pim/dav.rs`)
  — a small `server_err(status, body, cap)` helper.
