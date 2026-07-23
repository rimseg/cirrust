<script setup lang="ts">
import { computed, onMounted, onUnmounted } from "vue";
import { storeToRefs } from "pinia";
import { usePlayerStore } from "../stores/player";
import {
  Play,
  Pause,
  SkipBack,
  SkipForward,
  X,
  Volume2,
  Music,
  LoaderCircle,
} from "lucide-vue-next";

const player = usePlayerStore();
const { current, playing, currentTime, duration, volume, preparing, hasPrev, hasNext, error } =
  storeToRefs(player);

function fmt(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "0:00";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

const seekMax = computed(() => (duration.value > 0 ? duration.value : 0));

/** Whether the focused element takes text input (so Space types a space there). */
function isEditable(el: EventTarget | null): boolean {
  const n = el as HTMLElement | null;
  if (!n || typeof n.tagName !== "string") return false;
  const tag = n.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || n.isContentEditable;
}

// Space toggles play/pause globally while a track is loaded. Handled in the
// CAPTURE phase so it wins over the Files list / gallery Space handlers — but
// only when we actually claim it; otherwise the event falls through untouched
// (no track loaded, typing in a field, or a modifier is held).
function onGlobalKey(e: KeyboardEvent) {
  if (e.code !== "Space" && e.key !== " ") return;
  if (!current.value || isEditable(e.target) || e.ctrlKey || e.metaKey || e.altKey) return;
  e.preventDefault();
  e.stopPropagation();
  player.toggle();
}

onMounted(() => window.addEventListener("keydown", onGlobalKey, true));
onUnmounted(() => window.removeEventListener("keydown", onGlobalKey, true));
</script>

<template>
  <footer
    v-if="current"
    class="flex items-center gap-3 border-t border-line bg-surface px-4 py-2 text-ink"
  >
    <!-- Track -->
    <div class="flex w-60 min-w-0 items-center gap-2.5">
      <span class="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-surface-alt text-accent">
        <LoaderCircle v-if="preparing" class="h-4 w-4 animate-spin" />
        <Music v-else class="h-4 w-4" />
      </span>
      <span class="min-w-0">
        <span class="block truncate text-sm font-medium">{{ current.name }}</span>
        <span class="block truncate text-xs" :class="error ? 'text-negative' : 'text-ink-soft'">
          <template v-if="error">{{ error }}</template>
          <template v-else-if="preparing">Preparing…</template>
          <template v-else>{{ player.queue.length }} in queue</template>
        </span>
      </span>
    </div>

    <!-- Transport + seek -->
    <div class="flex min-w-0 flex-1 flex-col items-center gap-1">
      <div class="flex items-center gap-1">
        <button
          class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt hover:text-ink disabled:opacity-40"
          :disabled="!hasPrev && currentTime < 3"
          title="Previous"
          @click="player.prev()"
        >
          <SkipBack class="h-4 w-4" />
        </button>
        <button
          class="grid h-9 w-9 place-items-center rounded-full bg-accent text-white transition hover:bg-accent-strong"
          :title="playing ? 'Pause' : 'Play'"
          @click="player.toggle()"
        >
          <Pause v-if="playing" class="h-4 w-4" />
          <Play v-else class="h-4 w-4 translate-x-px" />
        </button>
        <button
          class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt hover:text-ink disabled:opacity-40"
          :disabled="!hasNext"
          title="Next"
          @click="player.next()"
        >
          <SkipForward class="h-4 w-4" />
        </button>
      </div>
      <div class="flex w-full max-w-xl items-center gap-2">
        <span class="w-9 shrink-0 text-right text-[11px] tabular-nums text-ink-soft">
          {{ fmt(currentTime) }}
        </span>
        <input
          type="range"
          min="0"
          :max="seekMax"
          step="0.1"
          :value="currentTime"
          class="h-1 min-w-0 flex-1 accent-[var(--color-accent)]"
          @input="player.seek(+($event.target as HTMLInputElement).value)"
        />
        <span class="w-9 shrink-0 text-[11px] tabular-nums text-ink-soft">
          {{ fmt(duration) }}
        </span>
      </div>
    </div>

    <!-- Volume + close -->
    <div class="flex w-60 items-center justify-end gap-2">
      <Volume2 class="h-4 w-4 shrink-0 text-ink-soft" />
      <input
        type="range"
        min="0"
        max="1"
        step="0.01"
        :value="volume"
        class="h-1 w-20 accent-[var(--color-accent)]"
        @input="player.setVolume(+($event.target as HTMLInputElement).value)"
      />
      <button
        class="rounded-lg p-1.5 text-ink-soft transition hover:bg-surface-alt hover:text-negative"
        title="Close player"
        @click="player.close()"
      >
        <X class="h-4 w-4" />
      </button>
    </div>
  </footer>
</template>
