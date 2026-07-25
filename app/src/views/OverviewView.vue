<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { homeDir } from "@tauri-apps/api/path";
import { account, files, sync } from "../api";
import type {
  Account,
  AccountInfo,
  ActivityItem,
  FileEntry,
  SyncFolder,
  SyncState,
} from "../api/types";
import { useSyncStore } from "../stores/sync";
import { useAuthStore } from "../stores/auth";
import { basename, formatSize, formatSpeed, relativeTime } from "../utils/format";
import {
  RefreshCw,
  Folder,
  FolderSearch,
  Check,
  Clock,
  Pause,
  WifiOff,
  TriangleAlert,
  Trash2,
  ArrowUp,
  ArrowDown,
  LoaderCircle,
  Sun,
  Moon,
  Monitor,
  Upload,
  Download,
  CircleCheck,
  CircleX,
  Pencil,
  Link as LinkIcon,
  CalendarDays,
  Users,
  Shield,
  MessageSquare,
  ArchiveRestore,
  Activity,
} from "lucide-vue-next";
import type { Component } from "vue";
import { getTheme, applyTheme, type Theme } from "../utils/theme";

const syncStore = useSyncStore();
const authStore = useAuthStore();
const { status, progress, folders, folderStats, conflicts, activity: syncActivity } =
  storeToRefs(syncStore);
const { accounts, account: activeAccount } = storeToRefs(authStore);

const autostart = ref(false);
const info = ref<AccountInfo | null>(null);
const serverActivity = ref<ActivityItem[]>([]);
const loading = ref(true);
const syncing = ref(false);

// ---- New folder + settings state -------------------------------------------
const localPath = ref("");
const remotePath = ref("");
const newFolderAccount = ref<string | null>(null);
const busy = ref(false);
const ignoreText = ref("");
const dismissMsg = ref<string | null>(null);
const dismissing = ref(false);

// ---- Appearance ------------------------------------------------------------
const theme = ref<Theme>(getTheme());
const themeOptions: { value: Theme; label: string; icon: any }[] = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
];
function setTheme(t: Theme) {
  theme.value = t;
  applyTheme(t);
}

async function toggleAutostart() {
  if (autostart.value) {
    await disable();
    autostart.value = false;
  } else {
    await enable();
    autostart.value = true;
  }
}

async function syncNow() {
  syncing.value = true;
  try {
    if (status.value.paused) await syncStore.setPaused(false);
    else await sync.now();
  } finally {
    setTimeout(() => (syncing.value = false), 800);
  }
}

const unlimited = computed(() => !info.value || info.value.quotaTotal <= 0);
const usedPct = computed(() => {
  if (!info.value || unlimited.value) return 0;
  return Math.min(100, (info.value.quotaUsed / info.value.quotaTotal) * 100);
});
const initials = computed(() =>
  (info.value?.displayName || "?")
    .split(/\s+/)
    .map((s) => s[0])
    .slice(0, 2)
    .join("")
    .toUpperCase(),
);
const serverHost = computed(() => {
  try {
    return new URL(info.value!.serverUrl).host;
  } catch {
    return info.value?.serverUrl ?? "";
  }
});

// ---- Sync state presentation ------------------------------------------------
const STATE: Record<SyncState, { label: string; cls: string; dot: string; icon: any }> = {
  idle: { label: "Up to date", cls: "text-positive", dot: "bg-positive", icon: Check },
  syncing: { label: "Syncing…", cls: "text-accent", dot: "bg-accent animate-pulse", icon: RefreshCw },
  paused: { label: "Paused", cls: "text-ink-soft", dot: "bg-ink-soft", icon: Pause },
  error: { label: "Sync error", cls: "text-negative", dot: "bg-negative", icon: TriangleAlert },
  offline: { label: "Offline", cls: "text-warning", dot: "bg-warning", icon: WifiOff },
};
const syncView = computed(() => STATE[status.value.state]);

function hostOnly(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
function accountLabel(id: string): string {
  const a = accounts.value.find((x) => x.id === id);
  return a ? `${a.username}@${hostOnly(a.serverUrl)}` : "unknown account";
}
function accountKindOf(id: string): Account["kind"] | null {
  return accounts.value.find((x) => x.id === id)?.kind ?? null;
}

/** Live status for one folder, from its meta + the current run state. */
function folderState(folder: { id: string; enabled: boolean; remotePath: string }) {
  const stat = folderStats.value.find((s) => s.id === folder.id);
  if (!folder.enabled) return { label: "Paused", icon: Pause, cls: "text-ink-soft" };
  if (status.value.state === "syncing" && status.value.activeFolder === folder.remotePath)
    return { label: "Syncing…", icon: LoaderCircle, cls: "text-accent", spin: true };
  if (stat?.lastSync)
    return { label: `Synced ${relativeTime(stat.lastSync)}`, icon: Check, cls: "text-positive" };
  return { label: "Waiting for first sync", icon: Clock, cls: "text-warning" };
}

function folderMeta(folder: { id: string }) {
  const stat = folderStats.value.find((s) => s.id === folder.id);
  if (!stat || stat.files === 0) return "";
  return `${stat.files.toLocaleString()} files · ${formatSize(stat.bytes)}`;
}

async function dismissIdentical() {
  dismissing.value = true;
  try {
    const n = await syncStore.dismissIdenticalConflicts();
    dismissMsg.value =
      n > 0
        ? `Removed ${n} identical cop${n === 1 ? "y" : "ies"}.`
        : "No identical copies found — the remaining conflicts have real differences.";
    setTimeout(() => (dismissMsg.value = null), 5000);
  } finally {
    dismissing.value = false;
  }
}

function saveIgnore() {
  const patterns = ignoreText.value
    .split("\n")
    .map((p) => p.trim())
    .filter(Boolean);
  syncStore.saveIgnorePatterns(patterns);
}

async function pickLocal() {
  const dir = await open({ directory: true, multiple: false });
  if (typeof dir === "string") {
    localPath.value = dir;
    if (!remotePath.value) remotePath.value = "/" + (dir.split("/").pop() ?? "");
  }
}

// ---- Folders on the server --------------------------------------------------
// The other half of "add a folder": a fresh client pulling its existing cloud
// folders down. Shown as its own section above "Synced folders".
const browsePath = ref("/");
const browseDirs = ref<FileEntry[]>([]);
const browseLoading = ref(false);
const addingRemote = ref<string | null>(null);

function cleanRemote(path: string): string {
  return "/" + path.replace(/^\/+|\/+$/g, "");
}

async function loadBrowse(path: string) {
  browseLoading.value = true;
  try {
    browsePath.value = cleanRemote(path);
    browseDirs.value = (await files.list(path)).filter((e) => e.isDir);
  } finally {
    browseLoading.value = false;
  }
}

function browseUp() {
  const p = browsePath.value.replace(/\/+$/, "");
  loadBrowse(p.slice(0, p.lastIndexOf("/")) || "/");
}

/** The sync pair mapped exactly to this server path, if any. */
function pairFor(path: string) {
  const p = cleanRemote(path);
  return folders.value.find((f) => f.remotePath === p);
}

/** The pair that already syncs this path as part of a parent folder, if any. */
function coveredBy(path: string) {
  const p = cleanRemote(path);
  return folders.value.find(
    (f) => f.remotePath !== p && (p + "/").startsWith(f.remotePath.replace(/\/$/, "") + "/"),
  );
}

/** Does a synced pair live somewhere underneath this server folder? */
function containsSynced(path: string): boolean {
  const p = cleanRemote(path);
  return folders.value.some(
    (f) => f.remotePath !== p && (f.remotePath + "/").startsWith(p.replace(/\/$/, "") + "/"),
  );
}

/** One row per server folder at the current browse level, with its sync state
 * attached — plus any synced pairs not visible at this level, so the single
 * list is always the complete picture. */
interface FolderRow {
  entry?: FileEntry;
  folder?: SyncFolder;
}
const folderRows = computed<FolderRow[]>(() => {
  const rows: FolderRow[] = browseDirs.value.map((e) => ({ entry: e, folder: pairFor(e.path) }));
  const visible = new Set(rows.filter((r) => r.folder).map((r) => r.folder!.id));
  for (const f of folders.value) {
    if (!visible.has(f.id)) rows.push({ folder: f });
  }
  return rows;
});

function rowName(row: FolderRow): string {
  return row.entry ? row.entry.name : row.folder!.remotePath;
}

/** Sync an existing server folder down into ~/Nextcloud/<name> (two-way). */
async function syncRemoteDir(path: string) {
  const p = cleanRemote(path);
  addingRemote.value = p;
  try {
    const name = p.split("/").filter(Boolean).pop() ?? "Nextcloud";
    const home = (await homeDir()).replace(/\/+$/, "");
    const acc = newFolderAccount.value ?? activeAccount.value?.id ?? null;
    await syncStore.addFolder(`${home}/Nextcloud/${name}`, p, acc, true);
  } finally {
    addingRemote.value = null;
  }
}

const addNotice = ref<string | null>(null);

async function addFolder() {
  if (!localPath.value || !remotePath.value) return;
  busy.value = true;
  try {
    const requested = cleanRemote(remotePath.value);
    const acc = newFolderAccount.value ?? activeAccount.value?.id ?? null;
    const folder = await syncStore.addFolder(localPath.value, remotePath.value, acc);
    // The backend refuses to sync a fresh pair into a server folder that
    // already has files — tell the user where their folder actually went.
    if (folder.remotePath !== requested) {
      addNotice.value = `${requested} already has files on the server — syncing to ${folder.remotePath} instead.`;
      setTimeout(() => (addNotice.value = null), 8000);
    }
    localPath.value = "";
    remotePath.value = "";
  } finally {
    busy.value = false;
  }
}

const overallPct = computed(() =>
  progress.value.bytesTotal > 0
    ? Math.min(100, (progress.value.bytesDone / progress.value.bytesTotal) * 100)
    : 0,
);

function fmtEta(secs: number | null): string {
  if (secs == null) return "";
  if (secs < 60) return `${secs}s left`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m ${secs % 60}s left`;
  return `${Math.floor(m / 60)}h ${m % 60}m left`;
}

// Icon + colour per sync-activity kind, shown on the left of each row.
const activityIcon: Record<string, { icon: Component; cls: string }> = {
  uploaded: { icon: Upload, cls: "text-accent" },
  downloaded: { icon: Download, cls: "text-positive" },
  verified: { icon: CircleCheck, cls: "text-positive" },
  deleted: { icon: Trash2, cls: "text-ink-soft" },
  conflict: { icon: TriangleAlert, cls: "text-warning" },
  error: { icon: CircleX, cls: "text-negative" },
};
const fallbackActivity = { icon: Activity, cls: "text-ink-soft" };

// Map a Nextcloud server-activity `type` (e.g. "file_created", "shared",
// "calendar_event") to an icon by keyword — the strings vary a lot across apps.
function serverActivityIcon(type: string): { icon: Component; cls: string } {
  const t = (type ?? "").toLowerCase();
  const has = (...w: string[]) => w.some((k) => t.includes(k));
  if (has("delete", "trash", "unshare")) return { icon: Trash2, cls: "text-negative" };
  if (has("restore")) return { icon: ArchiveRestore, cls: "text-positive" };
  if (has("share", "link", "public")) return { icon: LinkIcon, cls: "text-accent" };
  if (has("calendar", "event")) return { icon: CalendarDays, cls: "text-accent" };
  if (has("contact", "card")) return { icon: Users, cls: "text-accent" };
  if (has("comment", "mention")) return { icon: MessageSquare, cls: "text-ink-soft" };
  if (has("security", "password", "login", "2fa", "auth")) return { icon: Shield, cls: "text-warning" };
  if (has("create", "add", "upload", "new")) return { icon: Upload, cls: "text-positive" };
  if (has("change", "modif", "edit", "updat", "move", "rename")) return { icon: Pencil, cls: "text-accent" };
  return fallbackActivity;
}

onMounted(async () => {
  newFolderAccount.value = activeAccount.value?.id ?? accounts.value[0]?.id ?? null;
  syncStore.refreshFolders();
  syncStore.refreshStatus();
  syncStore.refreshActivity();
  syncStore.loadConflicts();
  loadBrowse("/").catch(() => {});
  await syncStore.loadSettings();
  ignoreText.value = syncStore.ignorePatterns.join("\n");
  try {
    autostart.value = await isEnabled().catch(() => false);
    info.value = await account.info();
    serverActivity.value = await account.activity();
  } finally {
    loading.value = false;
  }
});
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="flex-1 overflow-auto p-6">
      <p v-if="loading" class="text-sm text-ink-soft">Loading…</p>

      <div v-else class="mx-auto max-w-3xl space-y-5">
        <!-- Page heading — flows with the content instead of staying pinned. -->
        <div class="flex items-center justify-between gap-3">
          <div class="min-w-0">
            <h1 class="text-lg font-semibold text-ink">
              Welcome back<template v-if="info?.displayName">, {{ info.displayName }}</template>
            </h1>
            <p class="text-xs text-ink-soft">{{ serverHost || "Your Nextcloud" }}</p>
          </div>
          <div class="flex shrink-0 items-center gap-2">
            <button
              class="rounded-lg border border-line px-3 py-1.5 text-sm text-ink hover:bg-surface-alt"
              @click="syncStore.setPaused(!status.paused)"
            >
              {{ status.paused ? "Resume" : "Pause" }}
            </button>
            <button
              class="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-strong disabled:opacity-50"
              :disabled="status.state === 'syncing' || syncing"
              @click="syncNow"
            >
              <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': status.state === 'syncing' || syncing }" />
              {{ status.state === "syncing" ? "Syncing…" : "Sync now" }}
            </button>
          </div>
        </div>

        <!-- Account + Storage -->
        <div class="grid gap-4 sm:grid-cols-2">
          <section class="flex items-center gap-3 rounded-xl border border-line bg-surface p-4">
            <div class="grid h-12 w-12 shrink-0 place-items-center rounded-full bg-accent text-base font-semibold text-white">
              {{ initials }}
            </div>
            <div class="min-w-0">
              <div class="truncate text-sm font-medium text-ink">{{ info?.displayName || "—" }}</div>
              <div v-if="info?.email" class="truncate text-xs text-ink-soft">{{ info.email }}</div>
              <div class="truncate text-xs text-ink-soft">
                {{ info?.productName || "Nextcloud" }}<template v-if="info?.serverVersion"> {{ info.serverVersion }}</template>
              </div>
            </div>
          </section>

          <section class="rounded-xl border border-line bg-surface p-4">
            <div class="mb-2 flex items-baseline justify-between">
              <h2 class="text-sm font-medium text-ink">Storage</h2>
              <span class="text-xs text-ink-soft">
                {{ formatSize(info?.quotaUsed ?? 0) }}<template v-if="!unlimited"> of {{ formatSize(info!.quotaTotal) }}</template>
              </span>
            </div>
            <div class="h-2 overflow-hidden rounded-full bg-line">
              <div
                class="h-full rounded-full transition-[width]"
                :class="usedPct > 90 ? 'bg-negative' : 'bg-accent'"
                :style="{ width: (unlimited ? 4 : usedPct) + '%' }"
              />
            </div>
            <div class="mt-1 text-right text-xs text-ink-soft">
              <template v-if="unlimited">unlimited</template>
              <template v-else>{{ formatSize(info!.quotaFree) }} free</template>
            </div>
          </section>
        </div>

        <!-- Paused banner -->
        <div
          v-if="status.paused"
          class="rounded-xl border border-warning/30 bg-warning/10 px-4 py-2.5 text-sm text-warning"
        >
          Syncing is paused. Changes are not being transferred.
        </div>

        <!-- Sync status summary -->
        <section class="rounded-xl border border-line bg-surface p-4">
          <div class="flex items-center gap-3">
            <span class="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-surface-alt">
              <component :is="syncView.icon" class="h-4 w-4" :class="[syncView.cls, status.state === 'syncing' ? 'animate-spin' : '']" />
            </span>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2 text-sm font-medium" :class="syncView.cls">
                <span class="h-2 w-2 rounded-full" :class="syncView.dot" />
                {{ syncView.label }}
              </div>
              <!-- Why the sync isn't running matters more than the folder count. -->
              <div v-if="status.message" class="truncate text-xs text-ink-soft">
                {{ status.message }}
              </div>
              <div v-else class="truncate text-xs text-ink-soft">
                {{ folders.length }} synced folder{{ folders.length === 1 ? "" : "s" }}
                <template v-if="status.lastSync"> · last sync {{ relativeTime(status.lastSync) }}</template>
              </div>
            </div>
          </div>
        </section>

        <!-- Conflicts -->
        <section
          v-if="conflicts.length > 0"
          class="rounded-xl border border-warning/40 bg-warning/5 p-4"
        >
          <div class="mb-2 flex items-center justify-between">
            <h2 class="text-sm font-medium text-ink">Conflicts ({{ conflicts.length }})</h2>
            <button
              class="rounded-lg border border-line px-2.5 py-1 text-xs text-ink transition hover:bg-surface-alt disabled:opacity-50"
              :disabled="dismissing"
              title="Compare each copy with its original and remove the ones with identical content"
              @click="dismissIdentical"
            >
              {{ dismissing ? "Comparing…" : "Dismiss identical" }}
            </button>
          </div>
          <p class="mb-3 text-xs text-ink-soft">
            Both sides changed. Choose which version to keep for each file.
          </p>
          <p v-if="dismissMsg" class="mb-3 rounded-lg bg-positive/10 px-3 py-1.5 text-xs text-positive">
            {{ dismissMsg }}
          </p>
          <ul class="space-y-2">
            <li
              v-for="c in conflicts"
              :key="c.localPath"
              class="flex items-center gap-3 rounded-lg bg-surface px-3 py-2"
            >
              <span class="min-w-0 flex-1">
                <span class="block truncate text-sm text-ink">{{ c.originalName }}</span>
                <span class="block truncate text-xs text-ink-soft">{{ c.folderRemote }}</span>
              </span>
              <button
                class="rounded border border-line px-2 py-1 text-xs text-ink hover:bg-surface-alt"
                @click="syncStore.resolveConflict(c.localPath, 'local')"
              >
                Keep mine
              </button>
              <button
                class="rounded border border-line px-2 py-1 text-xs text-ink hover:bg-surface-alt"
                @click="syncStore.resolveConflict(c.localPath, 'remote')"
              >
                Keep server
              </button>
            </li>
          </ul>
        </section>

        <!-- Live transfer -->
        <section
          v-if="progress.active || progress.speed > 0"
          class="rounded-xl border border-accent/30 bg-accent/5 p-4"
        >
          <div v-if="progress.phase === 'scanning'" class="flex items-center gap-3">
            <LoaderCircle class="h-5 w-5 shrink-0 animate-spin text-accent" />
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5 text-sm text-ink">
                <FolderSearch class="h-4 w-4 shrink-0 text-ink-soft" />
                <span class="truncate">Scanning {{ progress.currentFile || "folders" }}…</span>
              </div>
              <div class="text-xs tabular-nums text-ink-soft">
                {{ progress.scanned.toLocaleString() }} items checked — comparing with local files
              </div>
            </div>
          </div>

          <template v-else>
            <div class="space-y-2">
              <div v-for="f in progress.activeFiles" :key="f.path" class="flex items-center gap-2">
                <span
                  class="grid h-5 w-5 shrink-0 place-items-center rounded-full text-white"
                  :class="f.direction === 'upload' ? 'bg-accent' : 'bg-positive'"
                >
                  <component :is="f.direction === 'upload' ? ArrowUp : ArrowDown" class="h-3 w-3" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs text-ink">{{ basename(f.path) }}</span>
                  <div class="mt-0.5 h-1 overflow-hidden rounded-full bg-line">
                    <div
                      class="h-full rounded-full bg-accent transition-[width] duration-500 ease-linear"
                      :style="{ width: (f.total > 0 ? Math.min(100, (f.done / f.total) * 100) : 0) + '%' }"
                    />
                  </div>
                </span>
                <span class="shrink-0 text-[11px] tabular-nums text-ink-soft">
                  {{ formatSize(f.done) }} / {{ formatSize(f.total) }}
                </span>
              </div>
              <!-- Verification: existing same-size files being compared with
                   the server — adopted in place, not downloaded. -->
              <p
                v-if="progress.verifyTotal > 0 && progress.verifyDone < progress.verifyTotal"
                class="text-xs text-ink-soft"
              >
                Checking {{ progress.verifyDone.toLocaleString() }} of
                {{ progress.verifyTotal.toLocaleString() }} existing files against the
                server — files are only downloaded if they differ.
              </p>
              <p
                v-else-if="progress.activeFiles.length === 0"
                class="text-xs text-ink-soft"
              >
                Preparing next files…
              </p>
            </div>

            <div
              v-if="progress.filesTotal > 0"
              class="mt-3 flex items-baseline justify-between text-xs text-ink-soft"
            >
              <span>
                {{ progress.filesDone }} of {{ progress.filesTotal }} files
                <span v-if="progress.activeFiles.length" class="text-accent">
                  · {{ progress.activeFiles.length }} in progress
                </span>
              </span>
              <span class="tabular-nums">
                {{ formatSpeed(progress.speed) }}
                · {{ formatSize(progress.bytesDone) }} / {{ formatSize(progress.bytesTotal) }}
                <template v-if="progress.etaSecs != null"> · {{ fmtEta(progress.etaSecs) }}</template>
              </span>
            </div>
            <div v-if="progress.filesTotal > 0" class="mt-1 h-2 overflow-hidden rounded-full bg-line">
              <div
                class="h-full rounded-full bg-positive transition-[width] duration-500 ease-linear"
                :style="{ width: overallPct + '%' }"
              />
            </div>
          </template>
        </section>

        <!-- Sync a new folder -->
        <section class="rounded-xl border border-line bg-surface p-4">
          <h2 class="mb-1 text-sm font-medium text-ink">Sync a new folder</h2>
          <p class="mb-3 text-xs text-ink-soft">
            Upload a folder from this computer and keep it in sync. If the server
            name is already taken by other files, a "<code>name 2</code>" folder
            is created instead — existing server data is never absorbed. To sync
            a folder that already exists in your cloud, use the Folders list
            below.
          </p>
          <div class="flex flex-col gap-2 sm:flex-row sm:items-center">
            <div class="flex min-w-0 flex-1 items-center gap-1">
              <input
                v-model="localPath"
                placeholder="Local folder (created if missing)"
                class="min-w-0 flex-1 rounded-lg border border-line bg-surface-alt px-3 py-2 text-sm text-ink outline-none focus:border-accent"
              />
              <button
                class="shrink-0 rounded-lg border border-line p-2 text-ink-soft hover:bg-surface-alt hover:text-ink"
                title="Choose an existing local folder"
                @click="pickLocal"
              >
                <FolderSearch class="h-4 w-4" />
              </button>
            </div>
            <span class="text-ink-soft" title="Changes sync in both directions">↔</span>
            <input
              v-model="remotePath"
              placeholder="/RemoteFolder"
              class="min-w-0 flex-1 rounded-lg border border-line bg-surface px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
            <button
              class="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-strong disabled:opacity-50"
              :disabled="!localPath || !remotePath || busy"
              @click="addFolder"
            >
              Add
            </button>
          </div>
          <p v-if="addNotice" class="mt-2 rounded-lg bg-warning/10 px-3 py-1.5 text-xs text-warning">
            {{ addNotice }}
          </p>
          <div v-if="accounts.length > 1" class="mt-2 flex items-center gap-2 text-xs">
            <span class="text-ink-soft">Sync to account</span>
            <select
              v-model="newFolderAccount"
              class="rounded-lg border border-line bg-surface px-2 py-1 text-xs text-ink outline-none focus:border-accent"
            >
              <option v-for="a in accounts" :key="a.id" :value="a.id">
                {{ a.username }}@{{ hostOnly(a.serverUrl) }} ({{ a.kind }})
              </option>
            </select>
          </div>
        </section>

        <!-- Folders: the server's folder tree with sync state overlaid — one
             list answers both "what's in my cloud" and "what's on this PC". -->
        <section class="rounded-xl border border-line bg-surface p-4">
          <h2 class="mb-1 text-sm font-medium text-ink">Folders</h2>
          <p class="mb-2 text-xs text-ink-soft">
            Your cloud folders and whether they sync to this computer. Syncing a
            folder downloads it into <code>~/Nextcloud/&lt;name&gt;</code> and
            keeps both sides in sync.
          </p>
          <div class="flex items-center gap-2 border-b border-line pb-2">
            <button
              class="rounded p-1 text-ink-soft hover:bg-surface-alt hover:text-ink disabled:opacity-40"
              :disabled="browsePath === '/'"
              title="Up one level"
              @click="browseUp"
            >
              <ArrowUp class="h-4 w-4" />
            </button>
            <span class="min-w-0 flex-1 truncate text-xs text-ink">
              {{ browsePath === "/" ? "All files" : browsePath }}
            </span>
            <button
              class="rounded p-1 text-ink-soft hover:bg-surface-alt hover:text-ink"
              title="Reload"
              @click="loadBrowse(browsePath)"
            >
              <RefreshCw class="h-4 w-4" />
            </button>
          </div>
          <p v-if="browseLoading && folderRows.length === 0" class="py-2 text-xs text-ink-soft">
            Loading…
          </p>
          <p v-else-if="folderRows.length === 0" class="py-2 text-xs text-ink-soft">
            No folders yet — add one above, or create folders in your cloud.
          </p>
          <ul v-else class="divide-y divide-line">
            <li
              v-for="row in folderRows"
              :key="row.folder?.id ?? row.entry!.path"
              class="flex items-center gap-3 py-2.5"
            >
              <Folder
                class="h-5 w-5 shrink-0"
                :class="row.folder || (row.entry && coveredBy(row.entry.path)) ? 'text-accent' : 'text-ink-soft'"
              />
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <button
                    v-if="row.entry"
                    class="truncate text-sm text-ink hover:underline"
                    title="Open folder"
                    @click="loadBrowse(row.entry.path)"
                  >
                    {{ rowName(row) }}
                  </button>
                  <span v-else class="truncate text-sm text-ink">{{ rowName(row) }}</span>
                  <span
                    v-if="row.folder && accounts.length > 1"
                    class="shrink-0 rounded-full bg-surface-alt px-2 py-0.5 text-[11px] text-ink-soft"
                    :title="accountKindOf(row.folder.accountId) ?? ''"
                  >
                    {{ accountLabel(row.folder.accountId) }}
                  </span>
                </div>

                <!-- Synced pair: destination + live status. -->
                <template v-if="row.folder">
                  <div class="truncate text-xs text-ink-soft">↔ {{ row.folder.localPath }}</div>
                  <div class="mt-0.5 flex items-center gap-1.5 text-xs">
                    <component
                      :is="folderState(row.folder).icon"
                      class="h-3.5 w-3.5 shrink-0"
                      :class="[folderState(row.folder).cls, folderState(row.folder).spin ? 'animate-spin' : '']"
                    />
                    <span :class="folderState(row.folder).cls">{{ folderState(row.folder).label }}</span>
                    <span v-if="folderMeta(row.folder)" class="text-ink-soft">· {{ folderMeta(row.folder) }}</span>
                  </div>
                </template>
                <!-- Not paired itself, but inside an already-synced folder. -->
                <div v-else-if="coveredBy(row.entry!.path)" class="truncate text-xs text-positive">
                  Synced as part of {{ coveredBy(row.entry!.path)!.remotePath }}
                </div>
                <div v-else class="truncate text-xs text-ink-soft">
                  Not synced on this computer<template v-if="containsSynced(row.entry!.path)"> · contains a synced folder</template>
                </div>
              </div>

              <!-- Controls: pause/remove for pairs, Sync for the rest. -->
              <template v-if="row.folder">
                <button
                  class="relative h-5 w-9 shrink-0 rounded-full transition"
                  :class="row.folder.enabled ? 'bg-accent' : 'bg-line'"
                  :title="row.folder.enabled ? 'Pause this folder' : 'Resume this folder'"
                  @click="syncStore.setFolderEnabled(row.folder.id, !row.folder.enabled)"
                >
                  <span
                    class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-[left]"
                    :class="row.folder.enabled ? 'left-[18px]' : 'left-0.5'"
                  />
                </button>
                <button
                  class="rounded p-1.5 text-ink-soft transition hover:text-negative"
                  title="Stop syncing (files are kept)"
                  @click="syncStore.removeFolder(row.folder.id)"
                >
                  <Trash2 class="h-4 w-4" />
                </button>
              </template>
              <button
                v-else-if="!coveredBy(row.entry!.path)"
                class="shrink-0 rounded-lg border border-line px-2.5 py-1 text-xs text-ink hover:bg-surface-alt disabled:opacity-50"
                :disabled="addingRemote !== null"
                @click="syncRemoteDir(row.entry!.path)"
              >
                {{ addingRemote === cleanRemote(row.entry!.path) ? "Adding…" : "Sync" }}
              </button>
            </li>
          </ul>
        </section>

        <!-- Ignore patterns -->
        <section class="rounded-xl border border-line bg-surface p-4">
          <h2 class="mb-1 text-sm font-medium text-ink">Ignore patterns</h2>
          <p class="mb-2 text-xs text-ink-soft">
            One per line. Files/folders matching these are never synced
            (e.g. <code>*.tmp</code>, <code>node_modules</code>, <code>.git</code>).
          </p>
          <textarea
            v-model="ignoreText"
            rows="3"
            placeholder="*.tmp&#10;node_modules&#10;.git"
            class="w-full resize-y rounded-lg border border-line bg-surface-alt px-3 py-2 font-mono text-xs text-ink outline-none focus:border-accent"
          />
          <div class="mt-2 text-right">
            <button
              class="rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-strong"
              @click="saveIgnore"
            >
              Save patterns
            </button>
          </div>
        </section>

        <!-- Appearance -->
        <section class="rounded-xl border border-line bg-surface p-4">
          <h2 class="mb-1 text-sm font-medium text-ink">Appearance</h2>
          <p class="mb-3 text-xs text-ink-soft">
            Choose a color theme, or follow your system / Plasma color scheme.
          </p>
          <div class="inline-flex rounded-lg border border-line p-0.5">
            <button
              v-for="opt in themeOptions"
              :key="opt.value"
              class="flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition"
              :class="theme === opt.value
                ? 'bg-accent text-white'
                : 'text-ink-soft hover:bg-surface-alt'"
              @click="setTheme(opt.value)"
            >
              <component :is="opt.icon" class="h-4 w-4" />
              {{ opt.label }}
            </button>
          </div>
        </section>

        <!-- Recent sync activity -->
        <section>
          <h2 class="mb-2 text-sm font-medium text-ink">Recent sync activity</h2>
          <p v-if="syncActivity.length === 0" class="text-sm text-ink-soft">Nothing yet.</p>
          <ul v-else class="divide-y divide-line rounded-xl border border-line bg-surface">
            <li v-for="(a, i) in syncActivity" :key="i" class="flex items-center gap-3 px-4 py-2">
              <component
                :is="(activityIcon[a.kind] ?? fallbackActivity).icon"
                class="h-4 w-4 shrink-0"
                :class="(activityIcon[a.kind] ?? fallbackActivity).cls"
              />
              <span class="min-w-0 flex-1 truncate text-sm text-ink" :title="a.path">
                {{ basename(a.path) }}
                <span v-if="a.message" class="text-ink-soft">— {{ a.message }}</span>
              </span>
              <span v-if="a.size > 0" class="shrink-0 text-xs text-ink-soft">{{ formatSize(a.size) }}</span>
              <span class="shrink-0 text-xs text-ink-soft">{{ relativeTime(a.time) }}</span>
            </li>
          </ul>
        </section>

        <!-- Server activity -->
        <section>
          <h2 class="mb-2 text-sm font-medium text-ink">Server activity</h2>
          <p v-if="serverActivity.length === 0" class="rounded-xl border border-line bg-surface px-4 py-3 text-sm text-ink-soft">
            No recent activity (or the Activity app isn't enabled).
          </p>
          <ul v-else class="divide-y divide-line rounded-xl border border-line bg-surface">
            <li v-for="(a, i) in serverActivity" :key="i" class="flex items-start gap-3 px-4 py-2.5">
              <component
                :is="serverActivityIcon(a.activityType).icon"
                class="mt-0.5 h-4 w-4 shrink-0"
                :class="serverActivityIcon(a.activityType).cls"
              />
              <div class="min-w-0 flex-1">
                <div class="flex items-start justify-between gap-3">
                  <span class="min-w-0 text-sm text-ink">{{ a.subject }}</span>
                  <span class="shrink-0 text-xs text-ink-soft">{{ relativeTime(a.time) }}</span>
                </div>
                <div v-if="a.message" class="truncate text-xs text-ink-soft">{{ a.message }}</div>
              </div>
            </li>
          </ul>
        </section>

        <!-- Preferences -->
        <section class="flex items-center justify-between rounded-xl border border-line bg-surface p-4">
          <div>
            <h2 class="text-sm font-medium text-ink">Start on login</h2>
            <p class="text-xs text-ink-soft">Launch minimized to the tray so folders keep syncing.</p>
          </div>
          <button
            class="relative h-5 w-9 shrink-0 rounded-full transition"
            :class="autostart ? 'bg-accent' : 'bg-line'"
            @click="toggleAutostart"
          >
            <span
              class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-[left]"
              :class="autostart ? 'left-[18px]' : 'left-0.5'"
            />
          </button>
        </section>
      </div>
    </div>
  </div>
</template>
