<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { CalendarDays, ChevronLeft, ChevronRight } from "lucide-vue-next";
import { isToday, monthGridDays, monthStart, parseYmd, sameDay, ymd } from "../utils/date";

// A small custom calendar date picker (replaces the native <input type="date">).
// The popover is teleported to <body> so the surrounding modal's overflow can't
// clip it, and a full-screen backdrop closes it on an outside click.
const props = defineProps<{ modelValue: string }>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const open = ref(false);
const trigger = ref<HTMLElement | null>(null);
const popStyle = ref<Record<string, string>>({});
const cursor = ref<Date>(monthStart(parseYmd(props.modelValue) ?? new Date()));

const selected = computed(() => parseYmd(props.modelValue));
const label = computed(() =>
  selected.value
    ? selected.value.toLocaleDateString(undefined, {
        weekday: "short",
        day: "numeric",
        month: "short",
        year: "numeric",
      })
    : "Select date",
);
const monthLabel = computed(() =>
  cursor.value.toLocaleDateString(undefined, { month: "long", year: "numeric" }),
);
const weekdayNames = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

const days = computed(() => monthGridDays(cursor.value));

async function openPicker() {
  cursor.value = monthStart(selected.value ?? new Date());
  open.value = true;
  await nextTick();
  const r = trigger.value?.getBoundingClientRect();
  if (!r) return;
  const W = 260;
  const H = 300;
  const left = Math.max(8, Math.min(r.left, window.innerWidth - W - 8));
  const top = r.bottom + 6 + H > window.innerHeight ? Math.max(8, r.top - H - 6) : r.bottom + 6;
  popStyle.value = { top: `${top}px`, left: `${left}px` };
}
function pick(d: Date) {
  emit("update:modelValue", ymd(d));
  open.value = false;
}
function step(delta: number) {
  cursor.value = new Date(cursor.value.getFullYear(), cursor.value.getMonth() + delta, 1);
}
function isSelected(d: Date): boolean {
  return !!selected.value && sameDay(selected.value, d);
}
function inMonth(d: Date): boolean {
  return d.getMonth() === cursor.value.getMonth();
}
</script>

<template>
  <button
    ref="trigger"
    type="button"
    class="flex w-full items-center gap-2 rounded-lg border border-line bg-canvas px-3 py-1.5 text-left text-sm outline-none focus:border-accent"
    :class="open ? 'border-accent' : ''"
    @click="openPicker"
  >
    <CalendarDays class="h-4 w-4 shrink-0 text-ink-soft" />
    <span :class="selected ? 'text-ink' : 'text-ink-soft'">{{ label }}</span>
  </button>

  <Teleport to="body">
    <div v-if="open" class="fixed inset-0 z-[70]" @click="open = false" />
    <div
      v-if="open"
      class="fixed z-[71] w-[260px] rounded-xl border border-line bg-surface p-3 shadow-2xl"
      :style="popStyle"
    >
      <div class="mb-2 flex items-center justify-between">
        <button type="button" class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="step(-1)">
          <ChevronLeft class="h-4 w-4" />
        </button>
        <span class="text-sm font-medium text-ink">{{ monthLabel }}</span>
        <button type="button" class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="step(1)">
          <ChevronRight class="h-4 w-4" />
        </button>
      </div>
      <div class="mb-1 grid grid-cols-7 text-center text-[11px] font-medium text-ink-soft">
        <span v-for="w in weekdayNames" :key="w">{{ w }}</span>
      </div>
      <div class="grid grid-cols-7 gap-0.5">
        <button
          v-for="d in days"
          :key="d.toISOString()"
          type="button"
          class="grid h-8 place-items-center rounded-md text-sm transition"
          :class="[
            isSelected(d)
              ? 'bg-accent font-medium text-white'
              : inMonth(d)
                ? 'text-ink hover:bg-surface-alt'
                : 'text-ink-soft/50 hover:bg-surface-alt',
            isToday(d) && !isSelected(d) ? 'ring-1 ring-inset ring-accent/50' : '',
          ]"
          @click="pick(d)"
        >
          {{ d.getDate() }}
        </button>
      </div>
    </div>
  </Teleport>
</template>
