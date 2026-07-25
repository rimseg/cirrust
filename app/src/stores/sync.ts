import { defineStore } from "pinia";
import { ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { sync } from "../api";
import type {
  Conflict,
  FolderStat,
  SyncActivity,
  SyncFolder,
  SyncProgress,
  SyncStatus,
} from "../api/types";

const emptyProgress: SyncProgress = {
  active: false,
  phase: "",
  scanned: 0,
  currentFile: "",
  activeFiles: [],
  filesDone: 0,
  filesTotal: 0,
  bytesDone: 0,
  bytesTotal: 0,
  verifyDone: 0,
  verifyTotal: 0,
  speed: 0,
  etaSecs: null,
};

export const useSyncStore = defineStore("sync", () => {
  const status = ref<SyncStatus>({
    state: "idle",
    activeFolder: null,
    message: null,
    lastSync: null,
    folderCount: 0,
    paused: false,
  });
  const progress = ref<SyncProgress>({ ...emptyProgress });
  const activity = ref<SyncActivity[]>([]);
  const folders = ref<SyncFolder[]>([]);
  const folderStats = ref<FolderStat[]>([]);
  const ignorePatterns = ref<string[]>([]);
  const conflicts = ref<Conflict[]>([]);

  let poll: number | null = null;
  const unlisteners: UnlistenFn[] = [];

  async function refreshStatus() {
    status.value = await sync.status();
  }

  async function refreshActivity() {
    activity.value = await sync.activity();
  }

  async function refreshFolders() {
    folders.value = await sync.listFolders();
    folderStats.value = await sync.folderStats();
  }

  async function addFolder(
    localPath: string,
    remotePath: string,
    accountId: string | null,
    mergeExisting = false,
  ): Promise<SyncFolder> {
    const folder = await sync.addFolder(localPath, remotePath, accountId, mergeExisting);
    folders.value.push(folder);
    await refreshFolders();
    await refreshStatus();
    return folder;
  }

  async function removeFolder(id: string) {
    await sync.removeFolder(id);
    folders.value = folders.value.filter((f) => f.id !== id);
    await refreshStatus();
  }

  async function setFolderEnabled(id: string, enabled: boolean) {
    await sync.setFolderEnabled(id, enabled);
    const f = folders.value.find((f) => f.id === id);
    if (f) f.enabled = enabled;
  }

  async function setPaused(paused: boolean) {
    await sync.setPaused(paused);
    await refreshStatus();
  }

  async function loadSettings() {
    ignorePatterns.value = (await sync.settings()).ignorePatterns;
  }

  async function saveIgnorePatterns(patterns: string[]) {
    await sync.setIgnorePatterns(patterns);
    ignorePatterns.value = patterns;
  }

  async function loadConflicts() {
    conflicts.value = await sync.conflicts();
  }

  async function resolveConflict(localPath: string, keep: "local" | "remote") {
    await sync.resolveConflict(localPath, keep);
    conflicts.value = conflicts.value.filter((c) => c.localPath !== localPath);
  }

  async function dismissIdenticalConflicts(): Promise<number> {
    const n = await sync.dismissIdenticalConflicts();
    await loadConflicts();
    return n;
  }

  async function startPolling() {
    if (poll !== null) return;
    refreshStatus();
    refreshActivity();
    progress.value = await sync.progress();

    unlisteners.push(
      await listen<SyncStatus>("sync://status", (e) => {
        status.value = e.payload;
        // A run finished/updated — refresh the activity log + folder stats.
        refreshActivity();
        refreshFolders();
      }),
      await listen<SyncProgress>("sync://progress", (e) => {
        progress.value = e.payload;
      }),
    );

    poll = window.setInterval(() => {
      refreshStatus();
    }, 5000);
  }

  function stopPolling() {
    if (poll !== null) {
      clearInterval(poll);
      poll = null;
    }
    unlisteners.forEach((u) => u());
    unlisteners.length = 0;
  }

  return {
    status,
    progress,
    activity,
    folders,
    folderStats,
    ignorePatterns,
    conflicts,
    refreshStatus,
    refreshActivity,
    refreshFolders,
    addFolder,
    removeFolder,
    setFolderEnabled,
    setPaused,
    loadSettings,
    saveIgnorePatterns,
    loadConflicts,
    resolveConflict,
    dismissIdenticalConflicts,
    startPolling,
    stopPolling,
  };
});
