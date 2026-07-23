// Thin, typed wrappers around the Tauri command layer. Every backend command
// is reachable from here so components never call `invoke` directly.

import { invoke } from "@tauri-apps/api/core";
import type {
  Account,
  AccountInfo,
  ActivityItem,
  AddressBookInfo,
  CalendarInfo,
  CalEvent,
  Conflict,
  Contact,
  ContactInput,
  EventInput,
  FileEntry,
  FolderStat,
  LoginFlowInit,
  ServerKind,
  SyncActivity,
  SyncFolder,
  Share,
  SyncProgress,
  SyncSettings,
  SyncStatus,
  TrashEntry,
} from "./types";

export const auth = {
  startLogin: (serverUrl: string) =>
    invoke<LoginFlowInit>("auth_start_login", { serverUrl }),

  /** Poll once; resolves to the account when approved, or `null` while pending. */
  pollLogin: (pollEndpoint: string, pollToken: string) =>
    invoke<Account | null>("auth_poll_login", { pollEndpoint, pollToken }),

  /** Connect a Nextcloud OR ownCloud account with an app password. */
  addManual: (serverUrl: string, username: string, password: string, kind: ServerKind) =>
    invoke<Account>("auth_add_manual", { serverUrl, username, password, kind }),

  listAccounts: () => invoke<Account[]>("auth_list_accounts"),
  activeAccount: () => invoke<Account | null>("auth_active_account"),
  setActiveAccount: (accountId: string) =>
    invoke<void>("auth_set_active_account", { accountId }),
  removeAccount: (accountId: string) => invoke<void>("auth_remove_account", { accountId }),
};

export const files = {
  list: (path: string) => invoke<FileEntry[]>("files_list", { path }),
  /** Recursively search names under `scope` ("/" = whole account). */
  search: (query: string, scope: string) =>
    invoke<FileEntry[]>("files_search", { query, scope }),
  remove: (path: string) => invoke<void>("files_delete", { path }),
  download: (path: string, localPath: string) =>
    invoke<void>("files_download", { path, localPath }),
  upload: (remoteDir: string, localPaths: string[]) =>
    invoke<void>("files_upload", { remoteDir, localPaths }),
  mkdir: (path: string) => invoke<void>("files_mkdir", { path }),
  move: (from: string, to: string) => invoke<void>("files_move", { from, to }),
  copy: (from: string, to: string) => invoke<void>("files_copy", { from, to }),
  readText: (path: string) => invoke<string>("files_read_text", { path }),
};

export const media = {
  /** Absolute local path of a synced copy of `path`, or null when not on disk. */
  localPath: (path: string) => invoke<string | null>("media_local_path", { path }),
  /** Local path of a synced file OR folder for `path` (for "reveal in files"). */
  revealPath: (path: string) => invoke<string | null>("media_reveal_path", { path }),
  /** Local path for `path`, downloading into the app cache when not synced. */
  cache: (path: string, etag?: string | null) =>
    invoke<string>("media_cache", { path, etag: etag ?? null }),
  /** Raw bytes of a media file (local copy when synced, else fetched). */
  bytes: (path: string) => invoke<ArrayBuffer>("media_bytes", { path }),
  /** Loopback `http://` URL that plays `path` (synced copy or cache download). */
  httpUrl: (path: string, etag?: string | null) =>
    invoke<string>("media_http_url", { path, etag: etag ?? null }),
};

export const trash = {
  list: () => invoke<TrashEntry[]>("trash_list"),
  restore: (trashId: string) => invoke<void>("trash_restore", { trashId }),
  remove: (trashId: string) => invoke<void>("trash_delete", { trashId }),
  empty: () => invoke<void>("trash_empty"),
};

export const account = {
  info: () => invoke<AccountInfo>("account_info"),
  activity: () => invoke<ActivityItem[]>("account_activity"),
};


export const sharing = {
  list: (path?: string) => invoke<Share[]>("shares_list", { path: path ?? null }),
  create: (path: string, password?: string, expireDate?: string) =>
    invoke<Share>("share_create", {
      path,
      password: password || null,
      expireDate: expireDate || null,
    }),
  remove: (id: string) => invoke<void>("share_delete", { id }),
};

export const caldav = {
  /** Calendars from cache (refreshes from the server when the cache is cold). */
  calendars: () => invoke<CalendarInfo[]>("caldav_calendars"),
  /** Force a server reconciliation; returns the current calendar list. */
  refresh: () => invoke<CalendarInfo[]>("caldav_refresh"),
  /** Cached events, optionally limited to the given calendar ids. */
  events: (calendarIds?: string[] | null) =>
    invoke<CalEvent[]>("caldav_events", { calendarIds: calendarIds ?? null }),
  /** Create (no href) or update (href+etag) an event; returns the saved event. */
  saveEvent: (
    calendarId: string,
    event: EventInput,
    href?: string | null,
    etag?: string | null,
  ) =>
    invoke<CalEvent>("caldav_save_event", {
      args: { calendarId, event, href: href ?? null, etag: etag ?? null },
    }),
  deleteEvent: (calendarId: string, href: string, etag?: string | null) =>
    invoke<void>("caldav_delete_event", { calendarId, href, etag: etag ?? null }),
};

export const carddav = {
  addressbooks: () => invoke<AddressBookInfo[]>("carddav_addressbooks"),
  refresh: () => invoke<AddressBookInfo[]>("carddav_refresh"),
  contacts: (addressbookIds?: string[] | null) =>
    invoke<Contact[]>("carddav_contacts", { addressbookIds: addressbookIds ?? null }),
  saveContact: (
    addressbookId: string,
    contact: ContactInput,
    href?: string | null,
    etag?: string | null,
  ) =>
    invoke<Contact>("carddav_save_contact", {
      args: { addressbookId, contact, href: href ?? null, etag: etag ?? null },
    }),
  deleteContact: (addressbookId: string, href: string, etag?: string | null) =>
    invoke<void>("carddav_delete_contact", { addressbookId, href, etag: etag ?? null }),
};

export const sync = {
  listFolders: () => invoke<SyncFolder[]>("sync_list_folders"),
  folderStats: () => invoke<FolderStat[]>("sync_folder_stats"),
  addFolder: (localPath: string, remotePath: string, accountId: string | null) =>
    invoke<SyncFolder>("sync_add_folder", { localPath, remotePath, accountId }),
  removeFolder: (id: string) => invoke<void>("sync_remove_folder", { id }),
  status: () => invoke<SyncStatus>("sync_status"),
  progress: () => invoke<SyncProgress>("sync_progress"),
  activity: () => invoke<SyncActivity[]>("sync_activity"),
  now: () => invoke<void>("sync_now"),
  setPaused: (paused: boolean) => invoke<void>("sync_set_paused", { paused }),
  setFolderEnabled: (id: string, enabled: boolean) =>
    invoke<void>("sync_set_folder_enabled", { id, enabled }),
  settings: () => invoke<SyncSettings>("sync_settings"),
  setIgnorePatterns: (patterns: string[]) =>
    invoke<void>("sync_set_ignore_patterns", { patterns }),
  conflicts: () => invoke<Conflict[]>("sync_conflicts"),
  resolveConflict: (localPath: string, keep: "local" | "remote") =>
    invoke<void>("sync_resolve_conflict", { localPath, keep }),
  /** Remove conflicted copies identical to their original; returns count. */
  dismissIdenticalConflicts: () =>
    invoke<number>("sync_dismiss_identical_conflicts"),
};
