import { acceptHMRUpdate, defineStore } from "pinia";
import { computed, ref } from "vue";
import type { FileEntry } from "../api/types";
import { media } from "../api";

// Global audio player backing the bottom PlayerBar.
//
// Playback goes through the **Web Audio API**, not an <audio> element: we fetch
// the whole file's bytes and `decodeAudioData` them into a raw PCM buffer, then
// play that buffer through the audio graph. WebKitGTK's <audio>/blob path streams
// through a decoder that rebuffers ~every second (audible cuts) and can't
// determine the file's length; decoding to a buffer plays cleanly and gives an
// exact duration. The bytes are the original file, so there's no quality loss.
export const usePlayerStore = defineStore("player", () => {
  const queue = ref<FileEntry[]>([]);
  const index = ref(0);
  const playing = ref(false);
  const currentTime = ref(0);
  const duration = ref(0);
  const volume = ref(1);
  const preparing = ref(false);
  const error = ref<string | null>(null);

  const current = computed<FileEntry | null>(() => queue.value[index.value] ?? null);
  const hasPrev = computed(() => index.value > 0);
  const hasNext = computed(() => index.value < queue.value.length - 1);

  // Web Audio graph, created lazily on the first play (a user gesture, so the
  // context is allowed to start).
  let ctx: AudioContext | null = null;
  let gain: GainNode | null = null;
  let buffer: AudioBuffer | null = null; // current track, decoded
  let source: AudioBufferSourceNode | null = null;

  // Position bookkeeping: `startOffset` is where in the buffer the current
  // source began, at context time `startCtxTime`. `pausedAt` holds the position
  // while stopped. `expectingStop` distinguishes our own stop() (pause / seek /
  // track change) from a track ending naturally.
  let startOffset = 0;
  let startCtxTime = 0;
  let pausedAt = 0;
  let expectingStop = false;
  let raf = 0;
  let loadToken = 0;

  function ensureCtx(): AudioContext {
    if (!ctx) {
      ctx = new AudioContext();
      gain = ctx.createGain();
      gain.gain.value = volume.value;
      gain.connect(ctx.destination);
    }
    return ctx;
  }

  /** Live playback position in seconds. */
  function position(): number {
    if (playing.value && ctx) return startOffset + (ctx.currentTime - startCtxTime);
    return pausedAt;
  }

  function stopSource() {
    if (source) {
      expectingStop = true;
      try {
        source.stop();
      } catch {
        /* already stopped */
      }
      source.disconnect();
      source = null;
    }
  }

  function onEnded() {
    if (expectingStop) {
      expectingStop = false;
      return;
    }
    // Reached the natural end of the track.
    playing.value = false;
    cancelAnimationFrame(raf);
    if (hasNext.value) next();
    else currentTime.value = duration.value;
  }

  /** Play `buffer` from `offset` seconds. */
  function startAt(offset: number) {
    if (!buffer || !ctx || !gain) return;
    stopSource();
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(gain);
    src.onended = onEnded;
    startOffset = Math.max(0, Math.min(offset, buffer.duration));
    startCtxTime = ctx.currentTime;
    src.start(0, startOffset);
    source = src;
    playing.value = true;
    void ctx.resume();
    tick();
  }

  function tick() {
    cancelAnimationFrame(raf);
    const loop = () => {
      if (!playing.value) return;
      currentTime.value = Math.min(position(), duration.value);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
  }

  async function loadCurrent() {
    const entry = current.value;
    if (!entry) return;
    const token = ++loadToken;
    preparing.value = true;
    error.value = null;
    stopSource();
    playing.value = false;
    cancelAnimationFrame(raf);
    try {
      const bytes = await media.bytes(entry.path);
      if (token !== loadToken) return;
      const ac = ensureCtx();
      // decodeAudioData detaches its input, so hand it a copy.
      const decoded = await ac.decodeAudioData(bytes.slice(0));
      if (token !== loadToken) return;
      buffer = decoded;
      duration.value = decoded.duration;
      pausedAt = 0;
      currentTime.value = 0;
      startAt(0);
    } catch (e: any) {
      if (token === loadToken) error.value = "Can't play this track.";
      console.error("[player] decode/load failed", e);
    } finally {
      if (token === loadToken) preparing.value = false;
    }
  }

  /** Start a fresh queue at `startIndex` (defaults to the first track). */
  function playQueue(tracks: FileEntry[], startIndex = 0) {
    if (tracks.length === 0) return;
    // Resume the context *synchronously inside this click* — the autoplay policy
    // only lets it start from within the user-gesture call stack, not after the
    // later `await`s in loadCurrent().
    void ensureCtx().resume();
    queue.value = tracks;
    index.value = Math.max(0, Math.min(startIndex, tracks.length - 1));
    loadCurrent();
  }

  /** Pause playback, keeping the current position (no-op if already paused). */
  function pause() {
    if (!playing.value) return;
    pausedAt = position();
    stopSource();
    playing.value = false;
    cancelAnimationFrame(raf);
    currentTime.value = pausedAt;
  }

  function toggle() {
    if (!buffer) return;
    if (playing.value) {
      pause();
    } else {
      void ensureCtx().resume(); // keep the gesture→resume chain valid
      startAt(pausedAt);
    }
  }

  function next() {
    if (!hasNext.value) return;
    index.value += 1;
    loadCurrent();
  }

  function prev() {
    // Restart the current track first, like most players; step back only when
    // already near the start.
    if (position() > 3 || !hasPrev.value) {
      seek(0);
      return;
    }
    index.value -= 1;
    loadCurrent();
  }

  function seek(t: number) {
    const clamped = Math.max(0, Math.min(t, duration.value));
    pausedAt = clamped;
    currentTime.value = clamped;
    if (playing.value) startAt(clamped);
  }

  function setVolume(v: number) {
    volume.value = v;
    if (gain) gain.gain.value = v;
  }

  function close() {
    stopSource();
    cancelAnimationFrame(raf);
    buffer = null;
    queue.value = [];
    index.value = 0;
    playing.value = false;
    currentTime.value = 0;
    duration.value = 0;
    pausedAt = 0;
    preparing.value = false;
    error.value = null;
    loadToken++;
  }

  return {
    queue,
    index,
    playing,
    currentTime,
    duration,
    volume,
    preparing,
    error,
    current,
    hasPrev,
    hasNext,
    playQueue,
    toggle,
    pause,
    next,
    prev,
    seek,
    setVolume,
    close,
  };
});

if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(usePlayerStore, import.meta.hot));
}
