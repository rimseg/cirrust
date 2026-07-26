<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { storeToRefs } from "pinia";
import { useAuthStore } from "../stores/auth";
import { useSyncStore } from "../stores/sync";
import type { Account, SyncState } from "../api/types";
import { formatSpeed } from "../utils/format";
import {
  LayoutDashboard,
  Folder,
  CalendarDays,
  Users,
  Trash2,
  ArrowUp,
  ArrowDown,
  ArrowUpDown,
  ChevronDown,
  Plus,
  Check,
  LogOut,
} from "lucide-vue-next";

const authStore = useAuthStore();
const syncStore = useSyncStore();
const router = useRouter();
const { account, accounts } = storeToRefs(authStore);
const { status, progress } = storeToRefs(syncStore);

const switcherOpen = ref(false);

onMounted(() => syncStore.startPolling());

const nav = [
  { to: "/overview", label: "Overview", icon: LayoutDashboard },
  { to: "/files", label: "Files", icon: Folder },
  { to: "/calendar", label: "Calendar", icon: CalendarDays },
  { to: "/contacts", label: "Contacts", icon: Users },
  { to: "/trash", label: "Trash", icon: Trash2 },
];

// Up, down, or both — the speed next to it is the combined rate either way.
const syncArrow = computed(() => {
  const up = progress.value.activeFiles.some((f) => f.direction === "upload");
  const down = progress.value.activeFiles.some((f) => f.direction === "download");
  return up && down ? ArrowUpDown : up ? ArrowUp : ArrowDown;
});

const dotColor = computed(() => {
  const map: Record<SyncState, string> = {
    idle: "bg-positive",
    syncing: "bg-accent animate-pulse",
    paused: "bg-ink-soft",
    error: "bg-negative",
    offline: "bg-warning",
  };
  return map[status.value.state];
});

function hostOf(a: Account | null): string {
  if (!a) return "";
  try {
    return new URL(a.serverUrl).host;
  } catch {
    return a.serverUrl;
  }
}

async function switchTo(id: string) {
  switcherOpen.value = false;
  if (id !== account.value?.id) await authStore.setActive(id);
}

function addAccount() {
  switcherOpen.value = false;
  router.push({ name: "login" });
}

async function logout() {
  switcherOpen.value = false;
  await authStore.logout();
  if (!account.value) router.push({ name: "login" });
}
</script>

<template>
  <header class="flex items-center gap-4 border-b border-line bg-surface px-4 py-2">
    <!-- Primary navigation (brand dropped to make room for more sections) -->
    <nav class="flex items-center gap-1">
      <RouterLink
        v-for="item in nav"
        :key="item.to"
        :to="item.to"
        class="flex items-center gap-2 rounded-lg px-3 py-1.5 text-sm text-ink transition hover:bg-surface-alt"
        active-class="bg-accent/10 text-accent font-medium"
      >
        <component :is="item.icon" class="h-4 w-4 shrink-0" />
        {{ item.label }}
      </RouterLink>
    </nav>

    <span class="flex-1" />

    <!-- Sync status -->
    <div class="flex items-center gap-2 text-xs text-ink-soft">
      <span class="h-2.5 w-2.5 rounded-full" :class="dotColor" />
      <template v-if="status.state === 'syncing' && progress.speed > 0">
        <component :is="syncArrow" class="h-3 w-3 text-accent" />
        <span class="tabular-nums text-accent">{{ formatSpeed(progress.speed) }}</span>
      </template>
      <template v-else>
        <span class="capitalize">{{ status.state }}</span>
        <span v-if="status.folderCount" class="hidden sm:inline">· {{ status.folderCount }} folders</span>
      </template>
    </div>

    <!-- Account switcher -->
    <div class="relative">
      <button
        class="flex items-center gap-2 rounded-lg px-2 py-1 text-left transition hover:bg-surface-alt"
        @click="switcherOpen = !switcherOpen"
      >
        <div class="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-accent text-xs font-semibold text-white">
          {{ (account?.username || "?").slice(0, 1).toUpperCase() }}
        </div>
        <div class="hidden min-w-0 sm:block">
          <div class="truncate text-sm text-ink">{{ account?.username || "Not signed in" }}</div>
          <div class="truncate text-xs text-ink-soft">{{ hostOf(account) }}</div>
        </div>
        <ChevronDown class="h-4 w-4 shrink-0 text-ink-soft" />
      </button>

      <div
        v-if="switcherOpen"
        class="absolute right-0 top-full z-40 mt-1 w-64 overflow-hidden rounded-lg border border-line bg-surface shadow-lg"
      >
        <button
          v-for="a in accounts"
          :key="a.id"
          class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm transition hover:bg-surface-alt"
          @click="switchTo(a.id)"
        >
          <div class="grid h-6 w-6 shrink-0 place-items-center rounded-full bg-surface-alt text-[10px] font-semibold text-ink-soft">
            {{ a.username.slice(0, 1).toUpperCase() }}
          </div>
          <span class="min-w-0 flex-1">
            <span class="block truncate text-ink">{{ a.username }}</span>
            <span class="block truncate text-xs text-ink-soft">{{ hostOf(a) }} · {{ a.kind }}</span>
          </span>
          <Check v-if="a.id === account?.id" class="h-4 w-4 shrink-0 text-accent" />
        </button>

        <div class="border-t border-line">
          <button
            class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-accent transition hover:bg-surface-alt"
            @click="addAccount"
          >
            <Plus class="h-4 w-4" /> Add account
          </button>
          <button
            class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-ink-soft transition hover:bg-surface-alt hover:text-negative"
            @click="logout"
          >
            <LogOut class="h-4 w-4" /> Disconnect {{ account?.username }}
          </button>
        </div>
      </div>
    </div>

    <!-- Click-away backdrop for the switcher -->
    <div v-if="switcherOpen" class="fixed inset-0 z-30" @click="switcherOpen = false" />
  </header>
</template>
