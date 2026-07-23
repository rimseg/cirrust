<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { ask } from "@tauri-apps/plugin-dialog";
import { trash } from "../api";
import type { TrashEntry } from "../api/types";
import { formatSize } from "../utils/format";
import { Trash2, ArchiveRestore, Folder, File, RefreshCw } from "lucide-vue-next";

const entries = ref<TrashEntry[]>([]);
const loading = ref(true);
const error = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    entries.value = await trash.list();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
}

async function restore(e: TrashEntry) {
  await trash.restore(e.trashId);
  entries.value = entries.value.filter((x) => x.trashId !== e.trashId);
}

async function removeForever(e: TrashEntry) {
  const ok = await ask(`Permanently delete “${e.name}”? This cannot be undone.`, {
    title: "Delete forever",
    kind: "warning",
  });
  if (!ok) return;
  await trash.remove(e.trashId);
  entries.value = entries.value.filter((x) => x.trashId !== e.trashId);
}

async function emptyAll() {
  const ok = await ask("Permanently delete everything in the trash bin?", {
    title: "Empty trash",
    kind: "warning",
  });
  if (!ok) return;
  await trash.empty();
  entries.value = [];
}

function deletedWhen(e: TrashEntry): string {
  return new Date(e.deletedAt * 1000).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// Right-click menu for a trash row.
const rowMenu = reactive({ open: false, x: 0, y: 0, entry: null as TrashEntry | null });
function openRowMenu(ev: MouseEvent, e: TrashEntry) {
  rowMenu.x = ev.clientX;
  rowMenu.y = ev.clientY;
  rowMenu.entry = e;
  rowMenu.open = true;
}
function menuRestore() {
  const e = rowMenu.entry;
  rowMenu.open = false;
  if (e) restore(e);
}
function menuDelete() {
  const e = rowMenu.entry;
  rowMenu.open = false;
  if (e) removeForever(e);
}

onMounted(load);
</script>

<template>
  <div class="flex h-full flex-col">
    <header class="flex items-center justify-between border-b border-line px-5 py-3">
      <h1 class="text-sm font-semibold text-ink">Trash</h1>
      <div class="flex items-center gap-2">
        <button
          class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt"
          title="Refresh"
          @click="load"
        >
          <RefreshCw class="h-4 w-4" :class="loading ? 'animate-spin' : ''" />
        </button>
        <button
          class="rounded-lg border border-negative/40 px-3 py-1.5 text-sm text-negative transition hover:bg-negative/10 disabled:opacity-50"
          :disabled="entries.length === 0"
          @click="emptyAll"
        >
          Empty trash
        </button>
      </div>
    </header>

    <div class="flex-1 overflow-auto">
      <p v-if="error" class="m-5 rounded-lg bg-negative/10 px-3 py-2 text-sm text-negative">{{ error }}</p>
      <p v-else-if="loading" class="p-5 text-sm text-ink-soft">Loading…</p>
      <div v-else-if="entries.length === 0" class="grid h-full place-items-center text-sm text-ink-soft">
        <div class="text-center">
          <Trash2 class="mx-auto mb-2 h-8 w-8 opacity-50" />
          The trash bin is empty.
        </div>
      </div>

      <ul v-else class="divide-y divide-line">
        <li
          v-for="e in entries"
          :key="e.trashId"
          class="group flex items-center gap-3 px-5 py-2.5 transition hover:bg-surface-alt"
          @contextmenu.prevent="openRowMenu($event, e)"
        >
          <component :is="e.isDir ? Folder : File" class="h-5 w-5 shrink-0 text-ink-soft" />
          <span class="min-w-0 flex-1">
            <span class="block truncate text-sm text-ink">{{ e.name }}</span>
            <span class="block truncate text-xs text-ink-soft">
              was in /{{ e.originalLocation.replace(/^\/+/, "") }} · deleted {{ deletedWhen(e) }}
            </span>
          </span>
          <span class="w-20 text-right text-xs text-ink-soft">
            {{ e.isDir ? "" : formatSize(e.size) }}
          </span>
          <div class="flex gap-1 opacity-0 transition group-hover:opacity-100">
            <button
              class="flex items-center gap-1 rounded px-2 py-1.5 text-xs text-accent transition hover:bg-surface"
              @click="restore(e)"
            >
              <ArchiveRestore class="h-4 w-4" /> Restore
            </button>
            <button
              class="rounded p-1.5 text-ink-soft transition hover:bg-surface hover:text-negative"
              title="Delete forever"
              @click="removeForever(e)"
            >
              <Trash2 class="h-4 w-4" />
            </button>
          </div>
        </li>
      </ul>
    </div>

    <!-- Trash row right-click menu -->
    <template v-if="rowMenu.open">
      <div class="fixed inset-0 z-40" @click="rowMenu.open = false" @contextmenu.prevent="rowMenu.open = false" />
      <div
        class="fixed z-50 w-44 overflow-hidden rounded-lg border border-line bg-surface py-1 shadow-lg"
        :style="{ top: `${rowMenu.y}px`, left: `${rowMenu.x}px` }"
      >
        <button
          class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt"
          @click="menuRestore"
        >
          <ArchiveRestore class="h-4 w-4 text-ink-soft" /> Restore
        </button>
        <button
          class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-negative hover:bg-negative/10"
          @click="menuDelete"
        >
          <Trash2 class="h-4 w-4" /> Delete forever
        </button>
      </div>
    </template>
  </div>
</template>
