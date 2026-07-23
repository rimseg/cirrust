<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { ask } from "@tauri-apps/plugin-dialog";
import { caldav } from "../api";
import type { CalEvent, CalendarInfo, EventInput } from "../api/types";
import {
  CalendarDays,
  List,
  RefreshCw,
  Plus,
  ChevronLeft,
  ChevronRight,
  MapPin,
  Trash2,
  X,
} from "lucide-vue-next";
import DatePicker from "../components/DatePicker.vue";
import { isToday, monthGridDays, monthStart, parseDateTime, ymd } from "../utils/date";

type Mode = "agenda" | "month";

const calendars = ref<CalendarInfo[]>([]);
const selected = ref<Set<string>>(new Set());
const events = ref<CalEvent[]>([]);
const mode = ref<Mode>("agenda");
const loading = ref(true);
const refreshing = ref(false);
const error = ref<string | null>(null);
const monthCursor = ref(monthStart(new Date()));

let timer: ReturnType<typeof setInterval> | null = null;

// ── data loading ──────────────────────────────────────────────────────────

async function load() {
  loading.value = true;
  error.value = null;
  try {
    calendars.value = await caldav.calendars();
    if (selected.value.size === 0) {
      selected.value = new Set(calendars.value.map((c) => c.id));
    }
    events.value = await caldav.events();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
  // Reconcile with the server in the background after the cached view shows.
  refresh(true);
}

async function refresh(silent = false) {
  if (refreshing.value) return;
  refreshing.value = true;
  if (!silent) error.value = null;
  try {
    const prevIds = new Set(calendars.value.map((c) => c.id));
    calendars.value = await caldav.refresh();
    const next = new Set(selected.value);
    // Show brand-new calendars by default; forget ones that disappeared.
    for (const c of calendars.value) if (!prevIds.has(c.id)) next.add(c.id);
    selected.value = new Set([...next].filter((id) => calendars.value.some((c) => c.id === id)));
    events.value = await caldav.events();
  } catch (e: any) {
    if (!silent) error.value = e?.message ?? String(e);
  } finally {
    refreshing.value = false;
  }
}

onMounted(() => {
  load();
  // Auto-refresh every 5 minutes while the view is open.
  timer = setInterval(() => refresh(true), 5 * 60 * 1000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
});

// ── calendar filter ───────────────────────────────────────────────────────

function toggleCalendar(id: string) {
  const next = new Set(selected.value);
  next.has(id) ? next.delete(id) : next.add(id);
  selected.value = next;
}

function colorOf(id: string): string {
  return calendars.value.find((c) => c.id === id)?.color || "#3b82f6";
}

const visibleEvents = computed(() =>
  events.value.filter((e) => selected.value.has(e.calendarId)),
);

// ── date helpers (shared math lives in utils/date.ts) ─────────────────────

function timeLabel(e: CalEvent): string {
  if (e.allDay) return "All day";
  return parseDateTime(e.start).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

// ── agenda ────────────────────────────────────────────────────────────────

const agenda = computed(() => {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const upcoming = visibleEvents.value
    .filter((e) => {
      const end = parseDateTime(e.end ?? e.start);
      return end >= today;
    })
    .sort((a, b) => parseDateTime(a.start).getTime() - parseDateTime(b.start).getTime());

  const groups: { key: string; label: string; items: CalEvent[] }[] = [];
  for (const e of upcoming) {
    const day = parseDateTime(e.start);
    const key = ymd(day);
    let g = groups.find((x) => x.key === key);
    if (!g) {
      g = {
        key,
        label: day.toLocaleDateString(undefined, {
          weekday: "long",
          month: "long",
          day: "numeric",
        }),
        items: [],
      };
      groups.push(g);
    }
    g.items.push(e);
  }
  return groups;
});

// ── month grid ────────────────────────────────────────────────────────────

const monthLabel = computed(() =>
  monthCursor.value.toLocaleDateString(undefined, { month: "long", year: "numeric" }),
);

const weeks = computed(() => {
  const days = monthGridDays(monthCursor.value);
  const rows: { date: Date; inMonth: boolean; events: CalEvent[] }[][] = [];
  for (let w = 0; w < 6; w++) {
    rows.push(
      days.slice(w * 7, w * 7 + 7).map((date) => ({
        date,
        inMonth: date.getMonth() === monthCursor.value.getMonth(),
        events: visibleEvents.value
          .filter((e) => eventCoversDay(e, date))
          .sort((a, b) => parseDateTime(a.start).getTime() - parseDateTime(b.start).getTime()),
      })),
    );
  }
  return rows;
});

function eventCoversDay(e: CalEvent, day: Date): boolean {
  const start = parseDateTime(e.start);
  const end = e.end ? parseDateTime(e.end) : start;
  const d0 = new Date(day.getFullYear(), day.getMonth(), day.getDate());
  const s0 = new Date(start.getFullYear(), start.getMonth(), start.getDate());
  // All-day DTEND is exclusive; timed events end the same day for grid purposes.
  const e0 = new Date(end.getFullYear(), end.getMonth(), end.getDate());
  const last = e.allDay ? new Date(e0.getTime() - 86400000) : e0;
  return d0 >= s0 && d0 <= (last < s0 ? s0 : last);
}

const weekdayNames = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
function stepMonth(delta: number) {
  monthCursor.value = new Date(
    monthCursor.value.getFullYear(),
    monthCursor.value.getMonth() + delta,
    1,
  );
}

// ── editor ────────────────────────────────────────────────────────────────

// Right-click menu for a month-grid day cell.
const dayMenu = reactive({ open: false, x: 0, y: 0, date: null as Date | null });
function openDayMenu(e: MouseEvent, date: Date) {
  dayMenu.x = e.clientX;
  dayMenu.y = e.clientY;
  dayMenu.date = date;
  dayMenu.open = true;
}
function newEventFromMenu() {
  const d = dayMenu.date;
  dayMenu.open = false;
  if (d) openNew(d);
}

const editorOpen = ref(false);
const editing = ref<CalEvent | null>(null);
const saving = ref(false);
const form = reactive({
  calendarId: "",
  summary: "",
  allDay: false,
  startDate: "",
  startTime: "09:00",
  endDate: "",
  endTime: "10:00",
  location: "",
  description: "",
});

function openNew(day?: Date) {
  editing.value = null;
  const base = day ?? new Date();
  const writable = calendars.value[0];
  Object.assign(form, {
    calendarId: writable?.id ?? "",
    summary: "",
    allDay: false,
    startDate: ymd(base),
    startTime: "09:00",
    endDate: ymd(base),
    endTime: "10:00",
    location: "",
    description: "",
  });
  editorOpen.value = true;
}

function openEdit(e: CalEvent) {
  editing.value = e;
  const start = parseDateTime(e.start);
  const end = e.end ? parseDateTime(e.end) : start;
  // Show the inclusive last day for all-day events (stored end is exclusive).
  const shownEnd = e.allDay ? new Date(end.getTime() - 86400000) : end;
  Object.assign(form, {
    calendarId: e.calendarId,
    summary: e.summary,
    allDay: e.allDay,
    startDate: ymd(start),
    startTime: e.allDay ? "09:00" : start.toTimeString().slice(0, 5),
    endDate: ymd(shownEnd < start ? start : shownEnd),
    endTime: e.allDay ? "10:00" : end.toTimeString().slice(0, 5),
    location: e.location ?? "",
    description: e.description ?? "",
  });
  editorOpen.value = true;
}

async function save() {
  if (!form.summary.trim() || !form.calendarId) return;
  saving.value = true;
  error.value = null;
  try {
    const input: EventInput = {
      summary: form.summary.trim(),
      location: form.location.trim() || null,
      description: form.description.trim() || null,
      allDay: form.allDay,
      start: form.allDay ? form.startDate : `${form.startDate}T${form.startTime}`,
      end: form.allDay ? form.endDate : `${form.endDate}T${form.endTime}`,
    };
    const saved = await caldav.saveEvent(
      form.calendarId,
      input,
      editing.value?.href,
      editing.value?.etag,
    );
    // Update local list without a full refetch.
    events.value = events.value.filter((e) => e.href !== saved.href).concat(saved);
    editorOpen.value = false;
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    saving.value = false;
  }
}

async function remove() {
  const e = editing.value;
  if (!e) return;
  const ok = await ask(`Delete “${e.summary}”?`, { title: "Delete event", kind: "warning" });
  if (!ok) return;
  saving.value = true;
  try {
    await caldav.deleteEvent(e.calendarId, e.href, e.etag);
    events.value = events.value.filter((x) => x.href !== e.href);
    editorOpen.value = false;
  } catch (err: any) {
    error.value = err?.message ?? String(err);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Toolbar -->
    <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-5 py-3">
      <h1 class="flex items-center gap-2 text-base font-semibold text-ink">
        <CalendarDays class="h-5 w-5 text-accent" /> Calendar
      </h1>

      <div class="flex items-center gap-1 rounded-lg bg-surface-alt p-0.5">
        <button
          class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-sm transition"
          :class="mode === 'agenda' ? 'bg-surface text-ink shadow-sm' : 'text-ink-soft'"
          @click="mode = 'agenda'"
        >
          <List class="h-4 w-4" /> Agenda
        </button>
        <button
          class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-sm transition"
          :class="mode === 'month' ? 'bg-surface text-ink shadow-sm' : 'text-ink-soft'"
          @click="mode = 'month'"
        >
          <CalendarDays class="h-4 w-4" /> Month
        </button>
      </div>

      <template v-if="mode === 'month'">
        <div class="flex items-center gap-1">
          <button class="rounded p-1.5 text-ink-soft hover:bg-surface-alt" @click="stepMonth(-1)">
            <ChevronLeft class="h-4 w-4" />
          </button>
          <span class="min-w-40 text-center text-sm font-medium text-ink">{{ monthLabel }}</span>
          <button class="rounded p-1.5 text-ink-soft hover:bg-surface-alt" @click="stepMonth(1)">
            <ChevronRight class="h-4 w-4" />
          </button>
        </div>
      </template>

      <span class="flex-1" />

      <button
        class="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-ink-soft transition hover:bg-surface-alt"
        :disabled="refreshing"
        title="Refresh"
        @click="refresh(false)"
      >
        <RefreshCw class="h-4 w-4" :class="refreshing ? 'animate-spin' : ''" /> Refresh
      </button>
      <button
        class="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white transition hover:opacity-90"
        @click="openNew()"
      >
        <Plus class="h-4 w-4" /> New event
      </button>
    </header>

    <!-- Calendar filter chips -->
    <div
      v-if="calendars.length > 1"
      class="flex flex-wrap items-center gap-2 border-b border-line bg-surface px-5 py-2"
    >
      <button
        v-for="c in calendars"
        :key="c.id"
        class="flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs transition"
        :class="selected.has(c.id) ? 'border-line bg-surface-alt text-ink' : 'border-line text-ink-soft opacity-50'"
        @click="toggleCalendar(c.id)"
      >
        <span class="h-2.5 w-2.5 rounded-full" :style="{ backgroundColor: c.color || '#3b82f6' }" />
        {{ c.displayName }}
      </button>
    </div>

    <p v-if="error" class="border-b border-line bg-negative/10 px-5 py-2 text-sm text-negative">
      {{ error }}
    </p>

    <!-- Body -->
    <div class="min-h-0 flex-1 overflow-auto">
      <div v-if="loading" class="flex h-40 items-center justify-center text-ink-soft">
        <RefreshCw class="h-5 w-5 animate-spin" />
      </div>

      <!-- Agenda -->
      <div v-else-if="mode === 'agenda'" class="mx-auto max-w-3xl px-5 py-4">
        <p v-if="agenda.length === 0" class="py-16 text-center text-sm text-ink-soft">
          No upcoming events.
        </p>
        <div v-for="g in agenda" :key="g.key" class="mb-5">
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-soft">
            {{ g.label }}
          </h2>
          <ul class="overflow-hidden rounded-xl border border-line">
            <li
              v-for="e in g.items"
              :key="e.href"
              class="flex cursor-pointer items-center gap-3 border-b border-line bg-surface px-3 py-2.5 last:border-b-0 hover:bg-surface-alt"
              @click="openEdit(e)"
            >
              <span class="h-8 w-1 rounded-full" :style="{ backgroundColor: colorOf(e.calendarId) }" />
              <span class="w-20 shrink-0 text-xs tabular-nums text-ink-soft">{{ timeLabel(e) }}</span>
              <span class="min-w-0 flex-1">
                <span class="block truncate text-sm text-ink">{{ e.summary || "(no title)" }}</span>
                <span v-if="e.location" class="flex items-center gap-1 truncate text-xs text-ink-soft">
                  <MapPin class="h-3 w-3 shrink-0" /> {{ e.location }}
                </span>
              </span>
            </li>
          </ul>
        </div>
      </div>

      <!-- Month grid -->
      <div v-else class="flex min-h-full flex-col px-3 py-3">
        <div class="grid grid-cols-7 border-b border-line">
          <div
            v-for="wd in weekdayNames"
            :key="wd"
            class="px-2 py-1 text-center text-xs font-medium text-ink-soft"
          >
            {{ wd }}
          </div>
        </div>
        <div class="grid flex-1 grid-cols-7 grid-rows-6">
          <div
            v-for="cell in weeks.flat()"
            :key="cell.date.toISOString()"
            class="min-h-24 border-b border-r border-line p-1 last:border-r-0"
            :class="cell.inMonth ? 'bg-surface' : 'bg-surface-alt/40'"
            @dblclick="openNew(cell.date)"
            @contextmenu.prevent="openDayMenu($event, cell.date)"
          >
            <div class="mb-1 flex justify-end">
              <span
                class="grid h-6 w-6 place-items-center rounded-full text-xs"
                :class="isToday(cell.date) ? 'bg-accent text-white' : cell.inMonth ? 'text-ink' : 'text-ink-soft'"
              >
                {{ cell.date.getDate() }}
              </span>
            </div>
            <button
              v-for="e in cell.events.slice(0, 4)"
              :key="e.href"
              class="mb-0.5 flex w-full items-center gap-1 truncate rounded px-1 py-0.5 text-left text-[11px] text-ink hover:bg-surface-alt"
              @click.stop="openEdit(e)"
            >
              <span class="h-2 w-2 shrink-0 rounded-full" :style="{ backgroundColor: colorOf(e.calendarId) }" />
              <span class="truncate">{{ e.summary || "(no title)" }}</span>
            </button>
            <span v-if="cell.events.length > 4" class="px-1 text-[10px] text-ink-soft">
              +{{ cell.events.length - 4 }} more
            </span>
          </div>
        </div>
      </div>
    </div>

    <!-- Day right-click menu -->
    <template v-if="dayMenu.open">
      <div class="fixed inset-0 z-40" @click="dayMenu.open = false" @contextmenu.prevent="dayMenu.open = false" />
      <div
        class="fixed z-50 w-44 overflow-hidden rounded-lg border border-line bg-surface py-1 shadow-lg"
        :style="{ top: `${dayMenu.y}px`, left: `${dayMenu.x}px` }"
      >
        <button
          class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt"
          @click="newEventFromMenu"
        >
          <Plus class="h-4 w-4 text-ink-soft" /> New event
        </button>
      </div>
    </template>

    <!-- Editor modal -->
    <div
      v-if="editorOpen"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      @click.self="editorOpen = false"
    >
      <div class="flex w-full max-w-lg flex-col overflow-hidden rounded-2xl bg-surface shadow-xl">
        <header class="flex items-center justify-between border-b border-line px-5 py-3">
          <h2 class="text-sm font-semibold text-ink">{{ editing ? "Edit event" : "New event" }}</h2>
          <button class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="editorOpen = false">
            <X class="h-5 w-5" />
          </button>
        </header>

        <div class="flex flex-col gap-3 overflow-auto px-5 py-4">
          <input
            v-model="form.summary"
            placeholder="Title"
            class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          />

          <div class="flex items-center gap-2">
            <select
              v-model="form.calendarId"
              class="flex-1 rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            >
              <option v-for="c in calendars" :key="c.id" :value="c.id">{{ c.displayName }}</option>
            </select>
            <label class="flex items-center gap-1.5 text-sm text-ink-soft">
              <input v-model="form.allDay" type="checkbox" class="accent-[var(--color-accent)]" />
              All day
            </label>
          </div>

          <div class="grid grid-cols-2 gap-2">
            <label class="text-xs text-ink-soft">
              Start
              <div class="mt-1"><DatePicker v-model="form.startDate" /></div>
            </label>
            <label v-if="!form.allDay" class="text-xs text-ink-soft">
              &nbsp;
              <input
                v-model="form.startTime"
                type="time"
                class="mt-1 w-full rounded-lg border border-line bg-canvas px-2 py-1.5 text-sm text-ink outline-none focus:border-accent"
              />
            </label>
          </div>
          <div class="grid grid-cols-2 gap-2">
            <label class="text-xs text-ink-soft">
              End
              <div class="mt-1"><DatePicker v-model="form.endDate" /></div>
            </label>
            <label v-if="!form.allDay" class="text-xs text-ink-soft">
              &nbsp;
              <input
                v-model="form.endTime"
                type="time"
                class="mt-1 w-full rounded-lg border border-line bg-canvas px-2 py-1.5 text-sm text-ink outline-none focus:border-accent"
              />
            </label>
          </div>

          <input
            v-model="form.location"
            placeholder="Location"
            class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          />
          <textarea
            v-model="form.description"
            placeholder="Notes"
            rows="3"
            class="w-full resize-none rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          />
        </div>

        <footer class="flex items-center justify-between border-t border-line px-5 py-3">
          <button
            v-if="editing"
            class="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-negative hover:bg-negative/10"
            :disabled="saving"
            @click="remove"
          >
            <Trash2 class="h-4 w-4" /> Delete
          </button>
          <span class="flex-1" />
          <button
            class="rounded-lg px-3 py-1.5 text-sm text-ink-soft hover:bg-surface-alt"
            @click="editorOpen = false"
          >
            Cancel
          </button>
          <button
            class="ml-2 rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-white transition hover:opacity-90 disabled:opacity-50"
            :disabled="saving || !form.summary.trim()"
            @click="save"
          >
            {{ saving ? "Saving…" : "Save" }}
          </button>
        </footer>
      </div>
    </div>
  </div>
</template>
