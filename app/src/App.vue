<script setup lang="ts">
import { onMounted } from "vue";
import { RouterView } from "vue-router";
import { storeToRefs } from "pinia";
import { useAuthStore } from "./stores/auth";
import { usePlayerStore } from "./stores/player";
import TopBar from "./components/TopBar.vue";
import PlayerBar from "./components/PlayerBar.vue";

const authStore = useAuthStore();
const { account } = storeToRefs(authStore);

onMounted(() => {
  authStore.refresh();
  // Prime the media decoder in the background so the first audio play or video
  // preview isn't stalled by GStreamer's one-time plugin scan. Deferred to idle
  // so it never competes with the initial render.
  const warm = () => void usePlayerStore().warmUp();
  if ("requestIdleCallback" in window) {
    requestIdleCallback(warm, { timeout: 3000 });
  } else {
    setTimeout(warm, 1500);
  }
});
</script>

<template>
  <div class="flex h-full w-full flex-col overflow-hidden bg-canvas">
    <TopBar v-if="account" />
    <main class="min-h-0 flex-1 overflow-hidden">
      <RouterView v-slot="{ Component }">
        <Transition name="view" mode="out-in">
          <!-- Key by active account so switching remounts the view with that
               account's data (Files/Overview/Trash browse the active one). -->
          <component :is="Component" :key="account?.id ?? 'anon'" />
        </Transition>
      </RouterView>
    </main>
    <PlayerBar />
  </div>
</template>
