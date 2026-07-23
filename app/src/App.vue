<script setup lang="ts">
import { onMounted } from "vue";
import { RouterView } from "vue-router";
import { storeToRefs } from "pinia";
import { useAuthStore } from "./stores/auth";
import TopBar from "./components/TopBar.vue";
import PlayerBar from "./components/PlayerBar.vue";

const authStore = useAuthStore();
const { account } = storeToRefs(authStore);

onMounted(() => authStore.refresh());
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
