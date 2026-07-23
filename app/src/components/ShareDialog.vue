<script setup lang="ts">
import { onMounted, ref } from "vue";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { sharing } from "../api";
import type { FileEntry, Share } from "../api/types";
import { Link, X } from "lucide-vue-next";

const props = defineProps<{ entry: FileEntry }>();
const emit = defineEmits<{ close: [] }>();

const links = ref<Share[]>([]);
const loading = ref(true);
const creating = ref(false);
const error = ref<string | null>(null);
const copiedId = ref<string | null>(null);

const password = ref("");
const expireDate = ref("");

async function refresh() {
  loading.value = true;
  error.value = null;
  try {
    const all = await sharing.list(props.entry.path);
    links.value = all.filter((s) => s.shareType === 3);
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
}

async function createLink() {
  creating.value = true;
  error.value = null;
  try {
    const share = await sharing.create(
      props.entry.path,
      password.value || undefined,
      expireDate.value || undefined,
    );
    links.value.unshift(share);
    password.value = "";
    expireDate.value = "";
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    creating.value = false;
  }
}

async function copy(share: Share) {
  if (!share.url) return;
  await writeText(share.url);
  copiedId.value = share.id;
  setTimeout(() => (copiedId.value = null), 1500);
}

async function revoke(share: Share) {
  await sharing.remove(share.id);
  links.value = links.value.filter((s) => s.id !== share.id);
}

onMounted(refresh);
</script>

<template>
  <div class="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4" @click.self="emit('close')">
    <div class="w-full max-w-md rounded-xl border border-line bg-surface shadow-xl">
      <div class="flex items-center gap-3 border-b border-line px-4 py-3">
        <Link class="h-5 w-5 text-accent" />
        <span class="min-w-0 flex-1 truncate text-sm font-medium text-ink">Share “{{ entry.name }}”</span>
        <button class="rounded p-1 text-ink-soft transition hover:bg-surface-alt" @click="emit('close')">
          <X class="h-5 w-5" />
        </button>
      </div>

      <div class="space-y-4 p-4">
        <p v-if="error" class="rounded-lg bg-negative/10 px-3 py-2 text-xs text-negative">{{ error }}</p>

        <!-- Existing links -->
        <div>
          <p v-if="loading" class="text-sm text-ink-soft">Loading…</p>
          <p v-else-if="links.length === 0" class="text-sm text-ink-soft">No public links yet.</p>
          <ul v-else class="space-y-2">
            <li
              v-for="s in links"
              :key="s.id"
              class="flex items-center gap-2 rounded-lg border border-line bg-surface-alt px-3 py-2"
            >
              <span class="min-w-0 flex-1 truncate text-xs text-ink" :title="s.url ?? ''">{{ s.url }}</span>
              <span v-if="s.expiration" class="shrink-0 text-[10px] text-ink-soft">
                until {{ s.expiration.slice(0, 10) }}
              </span>
              <button class="shrink-0 rounded px-2 py-1 text-xs text-accent hover:bg-surface" @click="copy(s)">
                {{ copiedId === s.id ? "Copied!" : "Copy" }}
              </button>
              <button class="shrink-0 rounded px-2 py-1 text-xs text-negative hover:bg-surface" @click="revoke(s)">
                Revoke
              </button>
            </li>
          </ul>
        </div>

        <!-- Create -->
        <div class="space-y-2 border-t border-line pt-3">
          <p class="text-xs font-medium text-ink">New public link</p>
          <div class="flex gap-2">
            <input
              v-model="password"
              type="password"
              placeholder="Password (optional)"
              class="flex-1 rounded-lg border border-line bg-surface px-2.5 py-1.5 text-sm outline-none focus:border-accent"
            />
            <input
              v-model="expireDate"
              type="date"
              class="rounded-lg border border-line bg-surface px-2.5 py-1.5 text-sm text-ink outline-none focus:border-accent"
            />
          </div>
          <button
            class="w-full rounded-lg bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-strong disabled:opacity-50"
            :disabled="creating"
            @click="createLink"
          >
            {{ creating ? "Creating…" : "Create link" }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
