// Types mirroring the Rust backend's serialized structs (camelCase).

export type ServerKind = "nextcloud" | "owncloud";

export interface Account {
  id: string;
  serverUrl: string;
  username: string;
  kind: ServerKind;
}

export interface LoginFlowInit {
  loginUrl: string;
  pollToken: string;
  pollEndpoint: string;
}

export interface FileEntry {
  name: string;
  /** Path relative to the DAV root, starting with `/`. Directories end with `/`. */
  path: string;
  isDir: boolean;
  /** For directories: recursive size, when the server reports it. */
  size: number;
  mtime: string | null;
  contentType: string | null;
  etag: string | null;
  /** Files inside a directory (recursive), when reported. */
  fileCount?: number | null;
  /** Sub-directories inside a directory (recursive), when reported. */
  dirCount?: number | null;
}

export interface SyncFolder {
  id: string;
  accountId: string;
  localPath: string;
  remotePath: string;
  enabled: boolean;
}

export type SyncState = "idle" | "syncing" | "paused" | "error" | "offline";

export interface SyncStatus {
  state: SyncState;
  activeFolder: string | null;
  message: string | null;
  lastSync: string | null;
  folderCount: number;
  paused: boolean;
}

export interface SyncSettings {
  paused: boolean;
  ignorePatterns: string[];
}

export interface Conflict {
  folderId: string;
  folderRemote: string;
  localPath: string;
  name: string;
  originalName: string;
  /** Absolute local path of the original file (the server's version). */
  originalPath: string;
  /** Size/mtime of the conflicted copy ("mine"). */
  localSize: number | null;
  localModified: string | null;
  /** Size/mtime of the original file (the server's version), when it exists. */
  serverSize: number | null;
  serverModified: string | null;
}

export interface ActiveFile {
  path: string;
  direction: "upload" | "download";
  done: number;
  total: number;
}

export interface SyncProgress {
  active: boolean;
  /** "scanning" while folders are walked, "transferring" during transfers. */
  phase: "" | "scanning" | "transferring";
  /** Remote entries discovered so far in the folder being scanned. */
  scanned: number;
  /** Folder being scanned (scan phase only). */
  currentFile: string;
  /** Every file in flight right now — one per concurrent transfer. */
  activeFiles: ActiveFile[];
  filesDone: number;
  filesTotal: number;
  bytesDone: number;
  bytesTotal: number;
  /** Same-size existing files compared against the server (adopted in place
   * when identical) — separate from the transfer totals. */
  verifyDone: number;
  verifyTotal: number;
  /** Bytes per second (smoothed). */
  speed: number;
  /** Estimated seconds until the run finishes, when computable. */
  etaSecs: number | null;
}

export interface FolderStat {
  id: string;
  files: number;
  bytes: number;
  lastSync: string | null;
}

export type ActivityKind =
  | "uploaded"
  | "downloaded"
  | "verified"
  | "deleted"
  | "conflict"
  | "error";

export interface SyncActivity {
  time: string;
  kind: ActivityKind;
  path: string;
  size: number;
  message: string | null;
}

export interface AccountInfo {
  displayName: string;
  email: string | null;
  serverUrl: string;
  serverVersion: string | null;
  productName: string | null;
  quotaUsed: number;
  /** -1/-3 means unlimited/unknown. */
  quotaTotal: number;
  quotaFree: number;
  quotaRelative: number;
}

export interface ActivityItem {
  subject: string;
  message: string | null;
  time: string;
  activityType: string;
  objectName: string | null;
}

export interface Share {
  id: string;
  /** 3 = public link, 0 = user, 1 = group, … */
  shareType: number;
  url: string | null;
  token: string | null;
  path: string;
  permissions: number;
  expiration: string | null;
  label: string | null;
  shareWith: string | null;
}

export interface TrashEntry {
  trashId: string;
  name: string;
  originalLocation: string;
  /** Unix seconds. */
  deletedAt: number;
  size: number;
  isDir: boolean;
}

// ── CalDAV (calendars + events) ───────────────────────────────────────────

export interface CalendarInfo {
  id: string;
  href: string;
  displayName: string;
  color: string | null;
  ctag: string;
}

export interface CalEvent {
  uid: string;
  /** DAV path relative to `/remote.php/dav/`; the object to PUT/DELETE. */
  href: string;
  etag: string;
  calendarId: string;
  summary: string;
  description: string | null;
  location: string | null;
  /** `YYYY-MM-DD` when all-day, else `YYYY-MM-DDTHH:MM:SS` (local wall-clock). */
  start: string;
  end: string | null;
  allDay: boolean;
  rrule: string | null;
  status: string | null;
}

/** Event fields sent to the editor. */
export interface EventInput {
  summary: string;
  description?: string | null;
  location?: string | null;
  start: string;
  end?: string | null;
  allDay: boolean;
}

// ── CardDAV (address books + contacts) ────────────────────────────────────

export interface AddressBookInfo {
  id: string;
  href: string;
  displayName: string;
  ctag: string;
}

export interface TypedValue {
  label: string;
  value: string;
}

export interface Contact {
  uid: string;
  href: string;
  etag: string;
  addressbookId: string;
  fullName: string;
  emails: TypedValue[];
  phones: TypedValue[];
  org: string | null;
  title: string | null;
  note: string | null;
}

export interface ContactInput {
  fullName: string;
  emails: TypedValue[];
  phones: TypedValue[];
  org?: string | null;
  title?: string | null;
  note?: string | null;
}

/** Serialized shape of `AppError` from the backend. */
export interface AppError {
  kind: string;
  message: string;
}
