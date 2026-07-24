<script setup lang="ts">
import { ref, watch } from "vue";
import { useRouter } from "vue-router";
import { storeToRefs } from "pinia";
import { useAuthStore } from "../stores/auth";
import type { ServerKind } from "../api/types";
import { Globe, KeyRound, ChevronLeft } from "lucide-vue-next";

const authStore = useAuthStore();
const { status, error, accounts } = storeToRefs(authStore);
const router = useRouter();

// This screen is also reached as "Add account", so allow going back if we
// already have at least one account connected.
const canGoBack = () => accounts.value.length > 0;

const mode = ref<"browser" | "manual">("browser");

// Browser (Nextcloud Login Flow v2)
const server = ref("");
function connectBrowser() {
  if (server.value.trim()) authStore.login(server.value.trim());
}

// Manual app password (Nextcloud OR ownCloud)
const mServer = ref("");
const mUser = ref("");
const mPass = ref("");
const mKind = ref<ServerKind>("nextcloud");
const busy = ref(false);

async function connectManual() {
  if (!mServer.value.trim() || !mUser.value.trim() || !mPass.value) return;
  busy.value = true;
  try {
    await authStore.addManual(mServer.value.trim(), mUser.value.trim(), mPass.value, mKind.value);
    router.push({ name: "overview" });
  } finally {
    busy.value = false;
  }
}

// Login Flow completes asynchronously — navigate once a new account lands.
watch(
  () => accounts.value.length,
  (n, prev) => {
    if (n > prev && mode.value === "browser") router.push({ name: "overview" });
  },
);
</script>

<template>
  <div class="grid h-full place-items-center p-6">
    <div class="w-full max-w-sm">
      <button
        v-if="canGoBack()"
        class="mb-4 flex items-center gap-1 text-xs text-ink-soft transition hover:text-ink"
        @click="router.back()"
      >
        <ChevronLeft class="h-4 w-4" /> Back
      </button>

      <div class="mb-6 text-center">
        <div class="mx-auto mb-4 grid h-14 w-14 place-items-center rounded-2xl bg-accent text-white">
          <!-- Cirrust brand mark (matches the app icon / favicon), not a
               generic cloud glyph. -->
          <svg class="h-8 w-8" viewBox="0 0 32 32" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round">
            <path d="M10 11q4-1.6 9 0" />
            <path d="M7 16.5q6.5-2.2 17 0" />
            <path d="M11 22q4-1.4 8.5 0" />
          </svg>
        </div>
        <h1 class="text-xl font-semibold text-ink">
          {{ canGoBack() ? "Add an account" : "Connect your cloud" }}
        </h1>
        <p class="mt-1 text-sm text-ink-soft">Nextcloud or ownCloud — sync as many as you like.</p>
      </div>

      <!-- Mode switch -->
      <div class="mb-4 grid grid-cols-2 gap-1 rounded-lg bg-surface-alt p-1 text-xs font-medium">
        <button
          class="flex items-center justify-center gap-1.5 rounded-md py-1.5 transition"
          :class="mode === 'browser' ? 'bg-surface text-accent shadow-sm' : 'text-ink-soft'"
          @click="mode = 'browser'"
        >
          <Globe class="h-3.5 w-3.5" /> Browser login
        </button>
        <button
          class="flex items-center justify-center gap-1.5 rounded-md py-1.5 transition"
          :class="mode === 'manual' ? 'bg-surface text-accent shadow-sm' : 'text-ink-soft'"
          @click="mode = 'manual'"
        >
          <KeyRound class="h-3.5 w-3.5" /> App password
        </button>
      </div>

      <!-- Browser (Nextcloud Login Flow) -->
      <form v-if="mode === 'browser'" class="space-y-3" @submit.prevent="connectBrowser">
        <input
          v-model="server"
          type="text"
          inputmode="url"
          placeholder="cloud.example.com"
          :disabled="status !== 'idle'"
          class="w-full rounded-lg border border-line bg-surface px-3 py-2.5 text-sm text-ink outline-none focus:border-accent"
        />
        <button
          type="submit"
          :disabled="status !== 'idle' || !server.trim()"
          class="w-full rounded-lg bg-accent px-3 py-2.5 text-sm font-medium text-white transition hover:bg-accent-strong disabled:opacity-50"
        >
          <span v-if="status === 'polling'">Waiting for browser approval…</span>
          <span v-else>Connect</span>
        </button>
        <p v-if="status === 'polling'" class="text-center text-xs text-ink-soft">
          A browser window opened — approve the login, then return here.
          <button type="button" class="ml-1 underline" @click="authStore.stopPolling()">Cancel</button>
        </p>
      </form>

      <!-- Manual app password (Nextcloud or ownCloud) -->
      <form v-else class="space-y-3" @submit.prevent="connectManual">
        <div class="grid grid-cols-2 gap-1 rounded-lg bg-surface-alt p-1 text-xs">
          <button
            type="button"
            class="rounded-md py-1.5 transition"
            :class="mKind === 'nextcloud' ? 'bg-surface font-medium text-ink shadow-sm' : 'text-ink-soft'"
            @click="mKind = 'nextcloud'"
          >
            Nextcloud
          </button>
          <button
            type="button"
            class="rounded-md py-1.5 transition"
            :class="mKind === 'owncloud' ? 'bg-surface font-medium text-ink shadow-sm' : 'text-ink-soft'"
            @click="mKind = 'owncloud'"
          >
            ownCloud
          </button>
        </div>
        <input v-model="mServer" placeholder="Server (cloud.example.com)" inputmode="url"
          class="w-full rounded-lg border border-line bg-surface px-3 py-2.5 text-sm text-ink outline-none focus:border-accent" />
        <input v-model="mUser" placeholder="Username"
          class="w-full rounded-lg border border-line bg-surface px-3 py-2.5 text-sm text-ink outline-none focus:border-accent" />
        <input v-model="mPass" type="password" placeholder="App password"
          class="w-full rounded-lg border border-line bg-surface px-3 py-2.5 text-sm text-ink outline-none focus:border-accent" />
        <button
          type="submit"
          :disabled="busy || !mServer.trim() || !mUser.trim() || !mPass"
          class="w-full rounded-lg bg-accent px-3 py-2.5 text-sm font-medium text-white transition hover:bg-accent-strong disabled:opacity-50"
        >
          {{ busy ? "Connecting…" : "Connect" }}
        </button>
        <p class="text-center text-[11px] text-ink-soft">
          Create an app password in your server's Settings → Security.
        </p>
      </form>

      <p v-if="error" class="mt-4 rounded-lg bg-negative/10 px-3 py-2 text-center text-xs text-negative">
        {{ error }}
      </p>
    </div>
  </div>
</template>
