<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { open, ask } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { files, media } from "../api";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { FileEntry } from "../api/types";
import { formatDate, formatSize, isAudio, previewKind } from "../utils/format";
import { downloadOrReveal } from "../utils/download";
import FilePreview from "../components/FilePreview.vue";
import ShareDialog from "../components/ShareDialog.vue";
import { usePlayerStore } from "../stores/player";
import {
  Folder,
  File,
  FileText,
  FileSpreadsheet,
  FileArchive,
  FileCode,
  FileImage,
  FileVideo,
  Presentation,
  Music,
  Link,
  Pencil,
  Download,
  Trash2,
  Upload,
  FolderPlus,
  RefreshCw,
  Copy,
  ListChecks,
  ArrowUp,
  ArrowDown,
  ChevronRight,
  MoreHorizontal,
  FolderOpen,
} from "lucide-vue-next";

type IconSpec = { icon: any; color: string };

// Group extensions to a descriptive icon + a distinct colour so file types are
// recognisable at a glance (like a file manager). Colours are inline (icons use
// currentColor) so they don't depend on the Tailwind palette being kept.
const EXT_ICONS: Record<string, IconSpec> = {};
const register = (spec: IconSpec, exts: string[]) => exts.forEach((e) => (EXT_ICONS[e] = spec));
register({ icon: FileText, color: "#e0574f" }, ["pdf"]);
register({ icon: FileText, color: "#4a7dc9" }, ["doc", "docx", "odt", "rtf", "pages"]);
register({ icon: FileSpreadsheet, color: "#3aa76d" }, ["xls", "xlsx", "ods", "csv", "tsv", "numbers"]);
register({ icon: Presentation, color: "#e08a3c" }, ["ppt", "pptx", "odp", "key"]);
register({ icon: FileArchive, color: "#d9a441" }, ["zip", "7z", "tar", "gz", "bz2", "xz", "rar", "zst"]);
register({ icon: FileImage, color: "#a05fb4" }, ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "heic", "tiff", "avif"]);
register({ icon: FileVideo, color: "#c9598f" }, ["mp4", "mkv", "mov", "avi", "webm", "m4v", "flv"]);
register({ icon: FileCode, color: "#6b8cc4" }, [
  "js", "ts", "jsx", "tsx", "vue", "py", "rs", "go", "c", "cpp", "h", "hpp",
  "java", "kt", "sh", "rb", "php", "json", "yaml", "yml", "toml", "xml", "html", "css", "sql",
]);
register({ icon: FileText, color: "#8a94a6" }, ["txt", "md", "log", "ini", "conf"]);

function iconFor(e: FileEntry): IconSpec {
  if (e.isDir) return { icon: Folder, color: "var(--color-accent)" };
  if (isAudio(e.name, e.contentType)) return { icon: Music, color: "var(--color-accent)" };
  const ext = e.name.includes(".") ? e.name.split(".").pop()!.toLowerCase() : "";
  if (EXT_ICONS[ext]) return EXT_ICONS[ext];
  // Fall back to broad content-kind detection for extension-less/unknown files.
  switch (previewKind(e.name, e.contentType)) {
    case "image":
      return { icon: FileImage, color: "#a05fb4" };
    case "video":
      return { icon: FileVideo, color: "#c9598f" };
    case "text":
      return { icon: FileText, color: "#8a94a6" };
    default:
      return { icon: File, color: "var(--color-ink-soft)" };
  }
}

const player = usePlayerStore();

const previewEntry = ref<FileEntry | null>(null);
const shareEntry = ref<FileEntry | null>(null);

// Sibling entries for the gallery: search results while searching, otherwise the
// entries in the SAME folder as the opened file (its expanded-tree children, or
// the current level) — so arrow-key navigation walks that folder's images.
const previewSiblings = computed<FileEntry[]>(() => {
  const e = previewEntry.value;
  if (!e) return [];
  if (searchResults.value) return searchResults.value;
  const parent = e.path.slice(0, e.path.lastIndexOf("/") + 1);
  return childrenOf[parent] ?? entries.value;
});

/** Play an audio file in the bottom bar, queueing the audio files shown next
 *  to it (in display order) so next/prev walks the folder. */
function playAudio(entry: FileEntry) {
  const tracks = rows.value
    .map((r) => r.entry)
    .filter((e) => !e.isDir && isAudio(e.name, e.contentType));
  const start = Math.max(0, tracks.findIndex((t) => t.path === entry.path));
  player.playQueue(tracks, start);
}

const currentPath = ref("/");
const entries = ref<FileEntry[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);
const search = ref("");
const busy = ref(false);

// The Files view is a single Dolphin-style detail/tree view: folders expand
// inline (you stay on the level), navigable by mouse or keyboard.
// Tree expansion state (keyed by remote path).
const expanded = ref<Set<string>>(new Set());
const childrenOf = reactive<Record<string, FileEntry[]>>({});
const loadingPaths = ref<Set<string>>(new Set());

/** Index of the keyboard-focused row within `rows` (arrow-key navigation). */
const focusIndex = ref(0);

const selectMode = ref(false);
const selected = ref<Set<string>>(new Set());

function toggleSelectMode() {
  selectMode.value = !selectMode.value;
  if (!selectMode.value) selected.value = new Set();
}
const renamingPath = ref<string | null>(null);
const renameValue = ref("");
const newFolderOpen = ref(false);
const newFolderName = ref("");
const dragOver = ref(false);

/** Path of the row whose "⋯" menu is open (null = none). */
// Row action menu: a single floating menu positioned where it was opened —
// at the cursor for right-click, or just under the ⋯ button.
const menuEntry = ref<FileEntry | null>(null);
const menuPos = ref({ x: 0, y: 0 });
function openMenu(entry: FileEntry, x: number, y: number) {
  const W = 176;
  const H = entry.isDir ? 224 : 264;
  menuEntry.value = entry;
  menuPos.value = {
    x: Math.max(8, Math.min(x, window.innerWidth - W - 8)),
    y: Math.max(8, Math.min(y, window.innerHeight - H - 8)),
  };
}
function openMenuAtCursor(entry: FileEntry, e: MouseEvent) {
  openMenu(entry, e.clientX, e.clientY);
}
function openMenuFromButton(entry: FileEntry, e: MouseEvent) {
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  openMenu(entry, r.right - 176, r.bottom + 4);
}
function closeMenu() {
  menuEntry.value = null;
}

let unlistenDrop: UnlistenFn | null = null;

const breadcrumb = computed(() => {
  const parts = currentPath.value.split("/").filter(Boolean);
  const crumbs = [{ label: "Home", path: "/" }];
  let acc = "";
  for (const part of parts) {
    acc += `/${part}`;
    crumbs.push({ label: part, path: `${acc}/` });
  }
  return crumbs;
});

type SortKey = "name" | "size" | "date";
const sortBy = ref<SortKey>("name");
const sortDir = ref<"asc" | "desc">("asc");

function setSort(key: SortKey) {
  if (sortBy.value === key) {
    sortDir.value = sortDir.value === "asc" ? "desc" : "asc";
  } else {
    sortBy.value = key;
    sortDir.value = key === "name" ? "asc" : "desc";
  }
}

/** Sort one directory listing by the active column (dirs always group first). */
function arrange(list: FileEntry[]): FileEntry[] {
  const dir = sortDir.value === "asc" ? 1 : -1;
  return [...list].sort((a, b) => {
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    let c = 0;
    switch (sortBy.value) {
      case "size":
        c = a.size - b.size;
        break;
      case "date":
        c = (a.mtime ?? "").localeCompare(b.mtime ?? "");
        break;
      default:
        c = a.name.localeCompare(b.name, undefined, { numeric: true });
    }
    return c * dir;
  });
}

// ---- Recursive search (server-side WebDAV SEARCH) --------------------------
// null = browsing the tree; an array = showing flat search results.
const searchResults = ref<FileEntry[] | null>(null);
const searching = ref(false);
let searchTimer: number | undefined;

function onSearchInput() {
  window.clearTimeout(searchTimer);
  if (!search.value.trim()) {
    searchResults.value = null;
    searching.value = false;
    return;
  }
  searchTimer = window.setTimeout(runSearch, 350);
}

async function runSearch() {
  const q = search.value.trim();
  if (!q) {
    searchResults.value = null;
    return;
  }
  searching.value = true;
  try {
    // Scope the search to where the user currently is.
    searchResults.value = await files.search(q, currentPath.value);
    focusIndex.value = 0;
    resetScroll();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
    searchResults.value = [];
  } finally {
    searching.value = false;
  }
}

/** Parent directory of a path (for the location label on search results). */
function parentDir(path: string): string {
  const p = path.replace(/\/$/, "");
  return p.slice(0, p.lastIndexOf("/") + 1) || "/";
}

/** Muted secondary text on a row: item count (tree) or location (search). */
function secondary(row: Row): string {
  if (searchResults.value !== null) {
    const d = parentDir(row.entry.path);
    return d === "/" ? "" : d;
  }
  return row.entry.isDir ? itemsLabel(row.entry) : "";
}

type Row = {
  entry: FileEntry;
  depth: number;
  expandable: boolean;
  expanded: boolean;
  loading: boolean;
};

/** Flattened, sorted rows — search results (flat) or the current level plus
 *  any inline-expanded folders. */
const rows = computed<Row[]>(() => {
  if (searchResults.value !== null) {
    return arrange(searchResults.value).map((e) => ({
      entry: e,
      depth: 0,
      expandable: false,
      expanded: false,
      loading: false,
    }));
  }
  const out: Row[] = [];
  const walk = (list: FileEntry[], depth: number) => {
    for (const e of arrange(list)) {
      const isExp = e.isDir && expanded.value.has(e.path);
      out.push({
        entry: e,
        depth,
        expandable: e.isDir,
        expanded: isExp,
        loading: loadingPaths.value.has(e.path),
      });
      if (isExp && childrenOf[e.path]) walk(childrenOf[e.path], depth + 1);
    }
  };
  walk(entries.value, 0);
  return out;
});

// Keep the focused row valid as the tree expands/collapses or gets filtered.
watch(
  () => rows.value.length,
  (n) => {
    if (focusIndex.value >= n) focusIndex.value = Math.max(0, n - 1);
  },
);

// The list is hidden while loading, so measure the viewport once it appears.
watch(loading, (l) => {
  if (!l) nextTick(measure);
});

const stats = computed(() => {
  let folders = 0;
  let filesCount = 0;
  let bytes = 0;
  for (const e of entries.value) {
    if (e.isDir) folders++;
    else filesCount++;
    bytes += e.size;
  }
  return { folders, files: filesCount, bytes };
});

function itemsLabel(e: FileEntry): string {
  const n = (e.fileCount ?? 0) + (e.dirCount ?? 0);
  if (e.fileCount == null && e.dirCount == null) return "";
  return `${n} item${n === 1 ? "" : "s"}`;
}

/** Toggle inline expansion of a folder in the tree view (lazy-loads children). */
async function toggleExpand(entry: FileEntry) {
  const p = entry.path;
  const s = new Set(expanded.value);
  if (s.has(p)) {
    s.delete(p);
    expanded.value = s;
    return;
  }
  s.add(p);
  expanded.value = s;
  if (!childrenOf[p]) {
    loadingPaths.value = new Set(loadingPaths.value).add(p);
    try {
      childrenOf[p] = await files.list(p);
    } catch (e: any) {
      const s2 = new Set(expanded.value);
      s2.delete(p);
      expanded.value = s2;
      error.value = e?.message ?? String(e);
    } finally {
      const l = new Set(loadingPaths.value);
      l.delete(p);
      loadingPaths.value = l;
    }
  }
}

async function load(path: string) {
  loading.value = true;
  error.value = null;
  selected.value = new Set();
  menuEntry.value = null;
  focusIndex.value = 0;
  // Navigating exits search and returns to the tree.
  window.clearTimeout(searchTimer);
  search.value = "";
  searchResults.value = null;
  searching.value = false;
  cancelRename();
  newFolderOpen.value = false;
  // Navigating changes what the tree roots on — drop stale expansion state.
  expanded.value = new Set();
  loadingPaths.value = new Set();
  for (const k of Object.keys(childrenOf)) delete childrenOf[k];
  try {
    entries.value = await files.list(path);
    currentPath.value = path;
    resetScroll();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
}

// ---- Virtual scrolling ----------------------------------------------------
// Uniform-height rows so we can render only the visible slice — large folders
// (thousands of entries) stay smooth instead of building a giant DOM.
const ROW_H = 44;
const OVERSCAN = 8;
const scrollEl = ref<HTMLElement | null>(null);
const searchInput = ref<HTMLInputElement | null>(null);
const scrollTop = ref(0);
const viewportH = ref(600);

const virtual = computed(() => {
  const total = rows.value.length;
  // Clamp so a shrunk list (collapse / new results) never scrolls past the end
  // and blanks the view.
  const maxScroll = Math.max(0, total * ROW_H - viewportH.value);
  const st = Math.min(scrollTop.value, maxScroll);
  const start = Math.max(0, Math.floor(st / ROW_H) - OVERSCAN);
  const end = Math.min(total, start + Math.ceil(viewportH.value / ROW_H) + OVERSCAN * 2);
  return {
    totalHeight: total * ROW_H,
    items: rows.value.slice(start, end).map((row, k) => ({ row, index: start + k })),
  };
});

// The ancestor-folder chain of the row currently at the top of the viewport, so
// that while you scroll deep inside an inline-expanded folder you always see
// where you are. Empty when browsing search results or at the top level.
const stickyCrumbs = computed(() => {
  if (searchResults.value !== null) return [];
  const list = rows.value;
  if (list.length === 0) return [];
  const topIdx = Math.min(list.length - 1, Math.floor(scrollTop.value / ROW_H));
  const top = list[topIdx];
  if (!top || top.depth === 0) return [];
  // Walk upward, picking the nearest preceding row at each shallower depth.
  const crumbs: { name: string; path: string; index: number }[] = [];
  let need = top.depth - 1;
  for (let j = topIdx - 1; j >= 0 && need >= 0; j--) {
    if (list[j].depth === need) {
      crumbs.push({ name: list[j].entry.name, path: list[j].entry.path, index: j });
      need--;
    }
  }
  return crumbs.reverse(); // root-first
});

/** Jump the list so row `index` sits at the top (used by the sticky crumbs). */
function scrollToIndex(index: number) {
  const el = scrollEl.value;
  if (!el) return;
  el.scrollTop = index * ROW_H;
  scrollTop.value = el.scrollTop;
}

function onScroll() {
  if (scrollEl.value) scrollTop.value = scrollEl.value.scrollTop;
}
function measure() {
  if (scrollEl.value) viewportH.value = scrollEl.value.clientHeight;
}
function resetScroll() {
  scrollTop.value = 0;
  nextTick(() => {
    if (scrollEl.value) scrollEl.value.scrollTop = 0;
  });
}

/** Move keyboard focus to row `i` (clamped) and scroll it into view. Rows are a
 *  fixed height, so the scroll offset is pure index math — done *synchronously*
 *  (no `nextTick`) so holding ↑/↓ stays snappy instead of lagging a frame per
 *  keypress. `scrollTop` is updated in the same tick so the virtual window
 *  recomputes immediately rather than waiting for the async scroll event. */
function focusRow(i: number) {
  const n = rows.value.length;
  if (n === 0) return;
  const idx = Math.max(0, Math.min(i, n - 1));
  focusIndex.value = idx;
  const el = scrollEl.value;
  if (!el) return;
  const top = idx * ROW_H;
  if (top < el.scrollTop) el.scrollTop = top;
  else if (top + ROW_H > el.scrollTop + el.clientHeight) {
    el.scrollTop = top + ROW_H - el.clientHeight;
  }
  scrollTop.value = el.scrollTop;
}

/** Type-ahead: focus the next entry whose name starts with `ch`, wrapping so a
 *  repeated key cycles through all matches. */
function typeAheadJump(ch: string) {
  const list = rows.value;
  if (list.length === 0) return;
  const needle = ch.toLowerCase();
  const start = focusIndex.value;
  for (let k = 1; k <= list.length; k++) {
    const i = (start + k) % list.length;
    if (list[i].entry.name.toLowerCase().startsWith(needle)) {
      focusRow(i);
      return;
    }
  }
}

/** Go up one directory (used by Left at the top level / Backspace). */
function goUp() {
  if (currentPath.value === "/") return;
  const trimmed = currentPath.value.replace(/\/$/, "");
  load(trimmed.slice(0, trimmed.lastIndexOf("/") + 1) || "/");
}

/** Refresh whatever is on screen after a mutation — the search results if
 *  searching, otherwise the current tree level (re-expanding open folders). */
async function reload() {
  if (searchResults.value !== null) {
    await runSearch();
    return;
  }
  const openPaths = [...expanded.value];
  await load(currentPath.value);
  // Best-effort: re-expand the folders that were open before.
  for (const p of openPaths) {
    const s = new Set(expanded.value);
    s.add(p);
    expanded.value = s;
    try {
      childrenOf[p] = await files.list(p);
    } catch {
      /* folder may be gone now — ignore */
    }
  }
}

function onRowClick(row: Row) {
  if (renamingPath.value) return;
  focusIndex.value = rows.value.findIndex((r) => r.entry.path === row.entry.path);
  if (selectMode.value) return toggle(row.entry);
  // Click a folder to enter it; the disclosure triangle expands it in place.
  if (row.entry.isDir) load(row.entry.path);
  else if (isAudio(row.entry.name, row.entry.contentType)) playAudio(row.entry);
  else if (previewKind(row.entry.name, row.entry.contentType) !== "none") {
    previewEntry.value = row.entry;
  }
}

function toggle(entry: FileEntry) {
  const s = new Set(selected.value);
  s.has(entry.path) ? s.delete(entry.path) : s.add(entry.path);
  selected.value = s;
}

const allSelected = computed(
  () => rows.value.length > 0 && rows.value.every((r) => selected.value.has(r.entry.path)),
);

function toggleAll() {
  selected.value = allSelected.value
    ? new Set()
    : new Set(rows.value.map((r) => r.entry.path));
}

async function bulkDownload() {
  const paths = new Set(selected.value);
  const targets = [...allEntries()].filter((e) => paths.has(e.path) && !e.isDir);
  if (targets.length === 0) return;
  const dir = await open({ directory: true, multiple: false, title: "Download into…" });
  if (typeof dir !== "string") return;
  busy.value = true;
  try {
    for (const e of targets) await files.download(e.path, `${dir}/${e.name}`);
    selected.value = new Set();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    busy.value = false;
  }
}

/** Every entry currently known (root + loaded tree children). */
function allEntries(): FileEntry[] {
  return [...entries.value, ...Object.values(childrenOf).flat()];
}

async function download(entry: FileEntry) {
  menuEntry.value = null;
  await downloadOrReveal(entry);
}

async function openInFileManager(entry: FileEntry) {
  menuEntry.value = null;
  try {
    const local = await media.revealPath(entry.path);
    if (local) await revealItemInDir(local);
    else error.value = `“${entry.name}” isn't synced to a local folder.`;
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
}

async function remove(entry: FileEntry) {
  menuEntry.value = null;
  const ok = await ask(`Delete “${entry.name}”? This cannot be undone.`, {
    title: "Delete",
    kind: "warning",
  });
  if (!ok) return;
  await files.remove(entry.path);
  await reload();
}

async function bulkDelete() {
  const n = selected.value.size;
  const ok = await ask(`Delete ${n} item${n > 1 ? "s" : ""}? This cannot be undone.`, {
    title: "Delete",
    kind: "warning",
  });
  if (!ok) return;
  busy.value = true;
  try {
    for (const path of selected.value) await files.remove(path);
    await reload();
  } finally {
    busy.value = false;
  }
}

function startRename(entry: FileEntry) {
  menuEntry.value = null;
  renamingPath.value = entry.path;
  renameValue.value = entry.name;
}
function cancelRename() {
  renamingPath.value = null;
  renameValue.value = "";
}
async function confirmRename(entry: FileEntry) {
  const name = renameValue.value.trim();
  if (!name || name === entry.name) return cancelRename();
  // Rename within the entry's own directory (works at any tree depth).
  const dir = entry.path.slice(0, entry.path.replace(/\/$/, "").lastIndexOf("/") + 1);
  const to = dir + name + (entry.isDir ? "/" : "");
  await files.move(entry.path, to);
  cancelRename();
  await reload();
}

async function createFolder() {
  const name = newFolderName.value.trim();
  if (!name) {
    newFolderOpen.value = false;
    return;
  }
  await files.mkdir(currentPath.value + name);
  newFolderName.value = "";
  newFolderOpen.value = false;
  await reload();
}

async function duplicate(entry: FileEntry) {
  menuEntry.value = null;
  const dir = entry.path.slice(0, entry.path.replace(/\/$/, "").lastIndexOf("/") + 1);
  const base = entry.name.replace(/(\.[^.]+)?$/, (ext) => ` (copy)${ext}`);
  const to = dir + base + (entry.isDir ? "/" : "");
  await files.copy(entry.path, to);
  await reload();
}

async function uploadDialog() {
  const picked = await open({ multiple: true });
  if (!picked) return;
  const paths = Array.isArray(picked) ? picked : [picked];
  await doUpload(paths);
}

async function doUpload(paths: string[]) {
  busy.value = true;
  try {
    await files.upload(currentPath.value, paths);
    await reload();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    busy.value = false;
  }
}

function onKey(e: KeyboardEvent) {
  // Ctrl/Cmd+F focuses the search box, wherever focus currently is.
  if ((e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F")) {
    e.preventDefault();
    searchInput.value?.focus();
    searchInput.value?.select();
    return;
  }
  if (e.key === "Escape") {
    if (menuEntry.value) { menuEntry.value = null; return; }
    if (previewEntry.value) { previewEntry.value = null; return; }
  }
  // Don't hijack keys while typing in a field, renaming, or with a dialog open.
  const tag = (document.activeElement?.tagName ?? "").toLowerCase();
  if (
    tag === "input" ||
    tag === "textarea" ||
    renamingPath.value ||
    newFolderOpen.value ||
    previewEntry.value ||
    shareEntry.value
  ) {
    return;
  }

  const row = rows.value[focusIndex.value];
  switch (e.key) {
    case "ArrowDown":
      e.preventDefault();
      focusRow(focusIndex.value + 1);
      break;
    case "ArrowUp":
      e.preventDefault();
      focusRow(focusIndex.value - 1);
      break;
    case "ArrowRight":
      if (!row?.entry.isDir) break;
      e.preventDefault();
      // Collapsed → expand in place; already expanded → step into first child.
      if (!row.expanded) toggleExpand(row.entry);
      else focusRow(focusIndex.value + 1);
      break;
    case "ArrowLeft":
      e.preventDefault();
      if (row?.entry.isDir && row.expanded) {
        toggleExpand(row.entry); // collapse
      } else if (row && row.depth > 0) {
        // Jump to the enclosing folder row.
        for (let i = focusIndex.value - 1; i >= 0; i--) {
          if (rows.value[i].depth < row.depth) {
            focusRow(i);
            break;
          }
        }
      } else {
        goUp(); // top level → leave this directory
      }
      break;
    case "Enter":
      if (!row) break;
      e.preventDefault();
      // Enter *enters* a folder (navigates in), plays audio, or previews a file.
      if (row.entry.isDir) load(row.entry.path);
      else if (isAudio(row.entry.name, row.entry.contentType)) playAudio(row.entry);
      else if (previewKind(row.entry.name, row.entry.contentType) !== "none") {
        previewEntry.value = row.entry;
      }
      break;
    case "Backspace":
      e.preventDefault();
      goUp();
      break;
    case " ":
      if (selectMode.value && row) {
        e.preventDefault();
        toggle(row.entry);
      }
      break;
    default:
      // Type-ahead: a bare letter/number jumps to the next matching entry.
      if (e.key.length === 1 && /[a-z0-9]/i.test(e.key) && !e.altKey && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        typeAheadJump(e.key);
      }
  }
}

onMounted(async () => {
  load("/");
  await nextTick();
  measure();
  window.addEventListener("keydown", onKey);
  window.addEventListener("resize", measure);
  unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "over" || p.type === "enter") dragOver.value = true;
    else if (p.type === "drop") {
      dragOver.value = false;
      if (p.paths?.length) doUpload(p.paths);
    } else dragOver.value = false;
  });
});
onUnmounted(() => {
  unlistenDrop?.();
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("resize", measure);
});
</script>

<template>
  <div class="relative flex h-full flex-col">
    <!-- Toolbar -->
    <header class="flex items-center gap-2 border-b border-line px-5 py-3">
      <nav class="flex min-w-0 flex-1 items-center gap-1 text-sm">
        <template v-for="(crumb, i) in breadcrumb" :key="crumb.path">
          <button
            class="max-w-40 truncate rounded px-1.5 py-0.5 hover:bg-surface-alt"
            :class="i === breadcrumb.length - 1 ? 'font-medium text-ink' : 'text-ink-soft'"
            @click="load(crumb.path)"
          >
            {{ crumb.label }}
          </button>
          <span v-if="i < breadcrumb.length - 1" class="text-ink-soft">/</span>
        </template>
      </nav>

      <button
        class="rounded-lg p-1.5 transition hover:bg-surface-alt"
        :class="selectMode ? 'bg-accent/10 text-accent' : 'text-ink-soft'"
        title="Select multiple files"
        @click="toggleSelectMode"
      >
        <ListChecks class="h-4 w-4" />
      </button>
      <input
        ref="searchInput"
        v-model="search"
        placeholder="Search all files…"
        class="w-44 rounded-lg border border-line bg-surface px-2.5 py-1.5 text-sm text-ink outline-none focus:border-accent"
        @input="onSearchInput"
        @keyup.enter="runSearch"
      />
      <button
        class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt disabled:opacity-50"
        title="Upload files"
        :disabled="busy"
        @click="uploadDialog"
      >
        <Upload class="h-4 w-4" />
      </button>
      <button
        class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt"
        title="New folder"
        @click="newFolderOpen = true"
      >
        <FolderPlus class="h-4 w-4" />
      </button>
      <button
        class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt"
        title="Refresh"
        @click="reload()"
      >
        <RefreshCw class="h-4 w-4" :class="loading ? 'animate-spin' : ''" />
      </button>
    </header>

    <!-- Selection bar -->
    <div
      v-if="selectMode"
      class="flex items-center gap-3 border-b border-line bg-accent/5 px-5 py-2 text-sm"
    >
      <label class="flex items-center gap-2 text-ink">
        <input
          type="checkbox"
          class="h-4 w-4 accent-[var(--color-accent)]"
          :checked="allSelected"
          @change="toggleAll"
        />
        {{ selected.size > 0 ? `${selected.size} selected` : "Select all" }}
      </label>
      <template v-if="selected.size > 0">
        <button class="text-accent hover:underline" :disabled="busy" @click="bulkDownload">
          Download
        </button>
        <button class="text-negative hover:underline" :disabled="busy" @click="bulkDelete">
          Delete
        </button>
        <button class="text-ink-soft hover:underline" @click="selected = new Set()">Clear</button>
      </template>
      <span class="flex-1" />
      <button class="text-xs text-ink-soft hover:underline" @click="toggleSelectMode">Done</button>
    </div>

    <div class="flex min-h-0 flex-1 flex-col">
      <p v-if="error" class="m-5 rounded-lg bg-negative/10 px-3 py-2 text-sm text-negative">{{ error }}</p>
      <p v-else-if="loading" class="p-5 text-sm text-ink-soft">Loading…</p>

      <template v-else>
      <!-- Column header — click to sort (fixed, above the scroll area) -->
      <div
        class="flex items-center gap-3 border-b border-line px-5 py-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-soft"
      >
        <span v-if="selectMode" class="w-4" />
        <span class="w-4" />
        <button
          class="flex min-w-0 flex-1 items-center gap-1 hover:text-ink"
          :class="{ 'text-ink': sortBy === 'name' }"
          @click="setSort('name')"
        >
          {{ searchResults !== null ? "Results" : "Name" }}
          <component :is="sortDir === 'asc' ? ArrowUp : ArrowDown" v-if="sortBy === 'name'" class="h-3 w-3" />
        </button>
        <button
          class="hidden w-40 items-center justify-end gap-1 hover:text-ink sm:flex"
          :class="{ 'text-ink': sortBy === 'date' }"
          @click="setSort('date')"
        >
          Modified
          <component :is="sortDir === 'asc' ? ArrowUp : ArrowDown" v-if="sortBy === 'date'" class="h-3 w-3" />
        </button>
        <button
          class="flex w-20 items-center justify-end gap-1 hover:text-ink"
          :class="{ 'text-ink': sortBy === 'size' }"
          @click="setSort('size')"
        >
          Size
          <component :is="sortDir === 'asc' ? ArrowUp : ArrowDown" v-if="sortBy === 'size'" class="h-3 w-3" />
        </button>
        <span class="w-10" />
      </div>

      <!-- New folder inline (fixed bar) -->
      <div v-if="newFolderOpen" class="flex items-center gap-3 border-b border-line bg-surface-alt px-5 py-2">
        <FolderPlus class="h-5 w-5 text-ink-soft" />
        <input
          v-model="newFolderName"
          autofocus
          placeholder="Folder name"
          class="flex-1 rounded border border-accent bg-surface px-2 py-1 text-sm outline-none"
          @keyup.enter="createFolder"
          @keyup.escape="newFolderOpen = false"
        />
        <button class="text-sm text-accent hover:underline" @click="createFolder">Create</button>
        <button class="text-sm text-ink-soft hover:underline" @click="newFolderOpen = false">Cancel</button>
      </div>

      <!-- Sticky location: the expanded-folder chain you're scrolled into. -->
      <div
        v-if="stickyCrumbs.length"
        class="flex items-center gap-1 border-b border-line bg-surface-alt/80 px-5 py-1.5 text-xs text-ink-soft"
      >
        <Folder class="h-3.5 w-3.5 shrink-0 opacity-70" />
        <template v-for="(c, i) in stickyCrumbs" :key="c.path">
          <button
            class="max-w-[14rem] truncate transition hover:text-ink hover:underline"
            :title="c.name"
            @click="scrollToIndex(c.index)"
          >
            {{ c.name }}
          </button>
          <ChevronRight v-if="i < stickyCrumbs.length - 1" class="h-3 w-3 shrink-0 opacity-50" />
        </template>
      </div>

      <!-- Virtualized row list -->
      <div ref="scrollEl" class="flex-1 overflow-auto" @scroll="onScroll" @contextmenu.prevent>
        <div v-if="rows.length === 0" class="px-5 py-6 text-center text-sm text-ink-soft">
          {{ searchResults !== null
            ? (searching ? "Searching…" : "No files match your search.")
            : "This folder is empty. Drop files here or use Upload." }}
        </div>

        <div v-else :style="{ height: `${virtual.totalHeight}px`, position: 'relative' }">
          <div
            v-for="v in virtual.items"
            :key="v.row.entry.path"
            :data-row-idx="v.index"
            class="group flex items-center gap-3 border-b border-line px-5"
            @contextmenu.prevent.stop="openMenuAtCursor(v.row.entry, $event)"
            :style="{
              position: 'absolute',
              top: `${v.index * 44}px`,
              left: 0,
              right: 0,
              height: '44px',
            }"
            :class="[
              v.index === focusIndex ? 'bg-surface-alt ring-1 ring-inset ring-accent/50' : 'hover:bg-surface-alt',
              selected.has(v.row.entry.path) ? 'bg-accent/5' : '',
            ]"
          >
            <input
              v-if="selectMode"
              type="checkbox"
              class="h-4 w-4 shrink-0 accent-[var(--color-accent)]"
              :checked="selected.has(v.row.entry.path)"
              @change="toggle(v.row.entry)"
            />

            <button
              class="flex min-w-0 flex-1 items-center gap-2 text-left"
              :style="{ paddingLeft: `${v.row.depth * 1.1}rem` }"
              @click="onRowClick(v.row)"
            >
              <!-- Disclosure triangle (folders only, tree mode) -->
              <span class="flex h-5 w-4 shrink-0 items-center justify-center">
                <RefreshCw v-if="v.row.loading" class="h-3 w-3 animate-spin text-ink-soft" />
                <ChevronRight
                  v-else-if="v.row.expandable"
                  class="h-4 w-4 text-ink-soft transition-transform"
                  :class="v.row.expanded ? 'rotate-90' : ''"
                  @click.stop="toggleExpand(v.row.entry)"
                />
              </span>
              <component
                :is="iconFor(v.row.entry).icon"
                class="h-5 w-5 shrink-0"
                :style="{ color: iconFor(v.row.entry).color }"
              />
              <span class="flex min-w-0 flex-1 items-baseline gap-1.5">
                <input
                  v-if="renamingPath === v.row.entry.path"
                  v-model="renameValue"
                  autofocus
                  class="w-full rounded border border-accent bg-surface px-2 py-0.5 text-sm outline-none"
                  @click.stop
                  @keyup.enter="confirmRename(v.row.entry)"
                  @keyup.escape="cancelRename"
                />
                <template v-else>
                  <span class="truncate text-sm text-ink">{{ v.row.entry.name }}</span>
                  <span v-if="secondary(v.row)" class="shrink-0 truncate text-xs text-ink-soft">
                    {{ secondary(v.row) }}
                  </span>
                </template>
              </span>
            </button>

            <span class="hidden w-40 truncate text-right text-xs text-ink-soft sm:block">
              {{ formatDate(v.row.entry.mtime) }}
            </span>
            <span class="w-20 text-right text-xs text-ink-soft">
              {{ v.row.entry.size > 0 ? formatSize(v.row.entry.size) : "" }}
            </span>

            <!-- Actions "⋯" — opens the shared floating menu under the button -->
            <div class="flex w-10 justify-end">
              <button
                class="rounded p-1.5 text-ink-soft transition hover:bg-surface hover:text-ink"
                :class="menuEntry?.path === v.row.entry.path ? 'bg-surface text-ink' : 'opacity-0 group-hover:opacity-100'"
                title="More actions"
                @click.stop="menuEntry?.path === v.row.entry.path ? closeMenu() : openMenuFromButton(v.row.entry, $event)"
              >
                <MoreHorizontal class="h-4 w-4" />
              </button>
            </div>
          </div>
        </div>
      </div>
      </template>
    </div>

    <!-- Shared row action menu: positioned where it was opened (cursor or ⋯). -->
    <template v-if="menuEntry">
      <div class="fixed inset-0 z-30" @click="closeMenu" @contextmenu.prevent="closeMenu" />
      <div
        class="fixed z-40 w-44 overflow-hidden rounded-lg border border-line bg-surface py-1 shadow-lg"
        :style="{ top: `${menuPos.y}px`, left: `${menuPos.x}px` }"
        @click.stop
      >
        <button class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt" @click="shareEntry = menuEntry; closeMenu()">
          <Link class="h-4 w-4 text-ink-soft" /> Share
        </button>
        <button class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt" @click="startRename(menuEntry)">
          <Pencil class="h-4 w-4 text-ink-soft" /> Rename
        </button>
        <button class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt" @click="duplicate(menuEntry)">
          <Copy class="h-4 w-4 text-ink-soft" /> Duplicate
        </button>
        <button v-if="!menuEntry.isDir" class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt" @click="download(menuEntry)">
          <Download class="h-4 w-4 text-ink-soft" /> Download
        </button>
        <button class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt" @click="openInFileManager(menuEntry)">
          <FolderOpen class="h-4 w-4 text-ink-soft" /> Open in file manager
        </button>
        <div class="my-1 border-t border-line" />
        <button class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-negative hover:bg-negative/10" @click="remove(menuEntry)">
          <Trash2 class="h-4 w-4" /> Delete
        </button>
      </div>
    </template>

    <!-- Status bar -->
    <footer
      v-if="!loading && entries.length > 0"
      class="border-t border-line bg-surface px-5 py-1.5 text-xs text-ink-soft"
    >
      {{ stats.folders }} folder{{ stats.folders === 1 ? "" : "s" }} ·
      {{ stats.files }} file{{ stats.files === 1 ? "" : "s" }}
      <template v-if="stats.bytes > 0"> · {{ formatSize(stats.bytes) }} total</template>
    </footer>

    <!-- Drop overlay -->
    <div
      v-if="dragOver"
      class="pointer-events-none absolute inset-0 z-10 m-3 grid place-items-center rounded-xl border-2 border-dashed border-accent bg-accent/10 text-accent"
    >
      <div class="text-sm font-medium">Drop to upload to {{ currentPath }}</div>
    </div>
    <div v-if="busy" class="absolute bottom-3 right-3 rounded-lg bg-ink/80 px-3 py-1.5 text-xs text-white">
      Working…
    </div>

    <Transition name="fade">
      <FilePreview
        v-if="previewEntry"
        :entry="previewEntry"
        :siblings="previewSiblings"
        @close="previewEntry = null"
      />
    </Transition>
    <Transition name="pop">
      <ShareDialog
        v-if="shareEntry"
        :entry="shareEntry"
        @close="shareEntry = null"
      />
    </Transition>
  </div>
</template>
