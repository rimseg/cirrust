<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { files } from "../api";
import type { FileEntry } from "../api/types";
import { formatSize, isImage, previewKind } from "../utils/format";
import { imageSrc, playableSrc } from "../utils/media";
import { downloadOrReveal } from "../utils/download";
import { usePlayerStore } from "../stores/player";
import { Download, X, ChevronLeft, ChevronRight, LoaderCircle, Maximize, Minimize } from "lucide-vue-next";

const player = usePlayerStore();

const props = defineProps<{ entry: FileEntry; siblings?: FileEntry[] }>();
const emit = defineEmits<{ close: [] }>();

const current = ref<FileEntry>(props.entry);
const text = ref("");
const loading = ref(false);
const error = ref<string | null>(null);

const kind = computed(() => previewKind(current.value.name, current.value.contentType));

// Resolved media URL. Images use the Range-capable `stream://` scheme; video
// uses `playableSrc()` → a loopback `http://127.0.0.1` URL (WebKitGTK can't seek
// a custom scheme — see mediahttp.rs). Both prefer the synced local copy.
// `preparing` shows a spinner while the source is resolved.
const url = ref("");
const preparing = ref(false);
let srcToken = 0;

// Fullscreen for video. WebKitGTK's NATIVE HTML5-video fullscreen renders a
// black frame (a separate accelerated path from the inline one we already fixed
// with WEBKIT_DISABLE_DMABUF_RENDERER). So we suppress it and instead fullscreen
// the whole app window via KWin — the preview overlay is already `fixed inset-0`,
// so the video keeps its working inline rendering while filling the screen.
const appWindow = getCurrentWindow();
const isFullscreen = ref(false);

async function setWindowFullscreen(on: boolean) {
  try {
    await appWindow.setFullscreen(on);
    isFullscreen.value = on;
  } catch (e) {
    console.warn("fullscreen toggle failed", e);
  }
}

function toggleFullscreen() {
  setWindowFullscreen(!isFullscreen.value);
}

/** Bounce WebKit's native (black) element-fullscreen back out into window
 * fullscreen, so the built-in `<video>` fullscreen button just works. */
function onNativeFullscreen() {
  const el = document.fullscreenElement;
  if (el) {
    document.exitFullscreen?.().catch(() => {});
    if (!isFullscreen.value) setWindowFullscreen(true);
  }
}

/** Revoke a previous blob URL if one was ever used (no-op for stream:// URLs). */
function releaseUrl() {
  if (url.value.startsWith("blob:")) URL.revokeObjectURL(url.value);
}

async function resolveSrc() {
  const entry = current.value;
  const k = kind.value;
  const token = ++srcToken;
  releaseUrl();
  url.value = "";
  if (k === "text" || k === "none") return;
  // A video has its own audio — pause the music player so they don't overlap.
  if (k === "video") player.pause();
  preparing.value = true;
  try {
    const resolved = k === "video" ? await playableSrc(entry) : await imageSrc(entry);
    if (token === srcToken) url.value = resolved;
    else if (resolved.startsWith("blob:")) URL.revokeObjectURL(resolved);
  } catch (e: any) {
    if (token === srcToken) error.value = e?.message ?? String(e);
  } finally {
    if (token === srcToken) preparing.value = false;
  }
}

// Gallery: flip through the sibling images in the same folder.
const gallery = computed(() =>
  kind.value === "image"
    ? (props.siblings ?? []).filter((e) => !e.isDir && isImage(e.name, e.contentType))
    : [],
);
const galleryIndex = computed(() => gallery.value.findIndex((e) => e.path === current.value.path));
const inGallery = computed(() => kind.value === "image" && gallery.value.length > 1);

/** Move `delta` positions through the gallery, wrapping around the ends. */
function step(delta: number) {
  const n = gallery.value.length;
  if (n <= 1) return;
  current.value = gallery.value[(galleryIndex.value + delta + n) % n];
}
const prev = () => step(-1);
const next = () => step(1);
function jumpTo(entry: FileEntry) {
  current.value = entry;
}

// A display URL for every gallery image (cheap — mostly local-path lookups) so
// the filmstrip thumbnails and neighbour preloads have a `src`; the bytes load
// lazily per <img>.
const thumbUrls = ref<Record<string, string>>({});
watch(
  gallery,
  async (list) => {
    const map: Record<string, string> = { ...thumbUrls.value };
    await Promise.all(
      list.map(async (e) => {
        if (!map[e.path]) {
          try {
            map[e.path] = await imageSrc(e);
          } catch {
            /* skip unresolvable thumbs */
          }
        }
      }),
    );
    thumbUrls.value = map;
    preloadNeighbours();
  },
  { immediate: true },
);

// Warm the browser cache for the neighbours so left/right feels instant.
function preloadNeighbours() {
  const n = gallery.value.length;
  if (n <= 1) return;
  for (const d of [-1, 1]) {
    const e = gallery.value[(galleryIndex.value + d + n) % n];
    const src = e && thumbUrls.value[e.path];
    if (src) {
      const im = new Image();
      im.src = src;
    }
  }
}

// Keep the active filmstrip thumbnail scrolled into view.
const strip = ref<HTMLElement | null>(null);
function scrollStripToCurrent() {
  nextTick(() => {
    strip.value
      ?.querySelector<HTMLElement>("[data-active='true']")
      ?.scrollIntoView({ inline: "center", block: "nearest", behavior: "smooth" });
  });
}

async function loadText() {
  if (kind.value !== "text") return;
  loading.value = true;
  error.value = null;
  try {
    text.value = await files.readText(current.value.path);
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
}

async function download() {
  await downloadOrReveal(current.value);
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    // Escape leaves fullscreen first; a second press closes the preview.
    if (isFullscreen.value) setWindowFullscreen(false);
    else emit("close");
    return;
  }
  if (
    (e.key === "f" || e.key === "F") &&
    kind.value === "video" &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey
  ) {
    e.preventDefault();
    toggleFullscreen();
    return;
  }
  // Arrow / Space / Home / End walk the image gallery. Only when viewing images
  // so PDFs and text keep their normal scroll behaviour.
  if (!inGallery.value) return;
  switch (e.key) {
    case "ArrowLeft":
    case "ArrowUp":
      e.preventDefault();
      prev();
      break;
    case "ArrowRight":
    case "ArrowDown":
    case " ":
      e.preventDefault();
      next();
      break;
    case "Home":
      e.preventDefault();
      jumpTo(gallery.value[0]);
      break;
    case "End":
      e.preventDefault();
      jumpTo(gallery.value[gallery.value.length - 1]);
      break;
  }
}

function requestClose() {
  if (isFullscreen.value) setWindowFullscreen(false);
  emit("close");
}

watch(current, () => {
  error.value = null;
  loadText();
  resolveSrc();
  preloadNeighbours();
  scrollStripToCurrent();
});
onMounted(() => {
  window.addEventListener("keydown", onKey);
  document.addEventListener("fullscreenchange", onNativeFullscreen);
  document.addEventListener("webkitfullscreenchange", onNativeFullscreen);
  loadText();
  resolveSrc();
  scrollStripToCurrent();
});
onUnmounted(() => {
  window.removeEventListener("keydown", onKey);
  document.removeEventListener("fullscreenchange", onNativeFullscreen);
  document.removeEventListener("webkitfullscreenchange", onNativeFullscreen);
  if (isFullscreen.value) appWindow.setFullscreen(false).catch(() => {});
  releaseUrl();
});
</script>

<template>
  <div class="fixed inset-0 z-50 flex flex-col bg-black/80" @click.self="requestClose">
    <div class="flex items-center gap-3 px-5 py-3 text-white">
      <span class="min-w-0 flex-1 truncate text-sm">{{ current.name }}</span>
      <span v-if="gallery.length > 1" class="shrink-0 text-xs text-white/60">
        {{ galleryIndex + 1 }} / {{ gallery.length }}
      </span>
      <span class="shrink-0 text-xs text-white/60">{{ formatSize(current.size) }}</span>
      <button
        v-if="kind === 'video'"
        class="rounded p-1.5 transition hover:bg-white/10"
        :title="isFullscreen ? 'Exit fullscreen (Esc)' : 'Fullscreen (F)'"
        @click="toggleFullscreen"
      >
        <Minimize v-if="isFullscreen" class="h-5 w-5" />
        <Maximize v-else class="h-5 w-5" />
      </button>
      <button class="rounded p-1.5 transition hover:bg-white/10" title="Download" @click="download">
        <Download class="h-5 w-5" />
      </button>
      <button class="rounded p-1.5 transition hover:bg-white/10" title="Close (Esc)" @click="requestClose">
        <X class="h-5 w-5" />
      </button>
    </div>

    <!-- Media area -->
    <div
      class="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden px-4 py-2"
      @click.self="requestClose"
    >
      <!-- gallery navigation: wide click zones, chevron appears on hover -->
      <button
        v-if="inGallery"
        class="group absolute inset-y-0 left-0 z-10 flex w-1/5 min-w-16 items-center justify-start pl-3"
        title="Previous (←)"
        @click.stop="prev"
      >
        <span class="grid h-11 w-11 place-items-center rounded-full bg-white/10 text-white opacity-0 transition group-hover:opacity-100">
          <ChevronLeft class="h-6 w-6" />
        </span>
      </button>
      <button
        v-if="inGallery"
        class="group absolute inset-y-0 right-0 z-10 flex w-1/5 min-w-16 items-center justify-end pr-3"
        title="Next (→)"
        @click.stop="next"
      >
        <span class="grid h-11 w-11 place-items-center rounded-full bg-white/10 text-white opacity-0 transition group-hover:opacity-100">
          <ChevronRight class="h-6 w-6" />
        </span>
      </button>

      <div v-if="preparing" class="flex flex-col items-center gap-3 text-white/70">
        <LoaderCircle class="h-8 w-8 animate-spin" />
        <p class="text-sm">Preparing…</p>
      </div>
      <img
        v-else-if="kind === 'image' && url"
        :key="current.path"
        :src="url"
        :alt="current.name"
        class="pfade max-h-full max-w-full rounded object-contain shadow-2xl"
      />
      <video
        v-else-if="kind === 'video' && url"
        :src="url"
        controls
        autoplay
        playsinline
        controlsList="nofullscreen"
        class="max-h-full max-w-full rounded"
        @dblclick="toggleFullscreen"
      />
      <embed
        v-else-if="kind === 'pdf' && url"
        :src="url"
        type="application/pdf"
        class="h-full w-full max-w-4xl rounded bg-white"
      />
      <div
        v-else-if="kind === 'text'"
        class="h-full w-full max-w-4xl overflow-auto rounded-lg bg-surface p-4"
        @click.stop
      >
        <p v-if="loading" class="text-sm text-ink-soft">Loading…</p>
        <p v-else-if="error" class="text-sm text-negative">{{ error }}</p>
        <pre v-else class="whitespace-pre-wrap break-words font-mono text-xs text-ink">{{ text }}</pre>
      </div>
      <div v-else class="text-center text-white/70">
        <p class="text-sm">No preview available for this file type.</p>
        <button
          class="mt-3 rounded-lg bg-white/10 px-3 py-1.5 text-sm text-white hover:bg-white/20"
          @click="download"
        >
          Download
        </button>
      </div>
    </div>

    <!-- Filmstrip -->
    <div
      v-if="inGallery"
      ref="strip"
      class="flex shrink-0 items-center gap-2 overflow-x-auto border-t border-white/10 bg-black/50 px-4 py-3"
    >
      <button
        v-for="e in gallery"
        :key="e.path"
        :data-active="e.path === current.path"
        class="relative shrink-0 overflow-hidden rounded-md ring-2 transition"
        :class="e.path === current.path ? 'ring-accent' : 'ring-transparent hover:ring-white/40'"
        :title="e.name"
        @click="jumpTo(e)"
      >
        <img
          :src="thumbUrls[e.path]"
          loading="lazy"
          :alt="e.name"
          class="h-14 w-14 bg-white/5 object-cover"
        />
      </button>
    </div>
  </div>
</template>
