<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { ask } from "@tauri-apps/plugin-dialog";
import { carddav } from "../api";
import type { AddressBookInfo, Contact, ContactInput, TypedValue } from "../api/types";
import {
  Users,
  RefreshCw,
  Plus,
  Search,
  Mail,
  Phone,
  Building2,
  Pencil,
  Trash2,
  X,
} from "lucide-vue-next";

const addressbooks = ref<AddressBookInfo[]>([]);
const contacts = ref<Contact[]>([]);
const query = ref("");
const activeAb = ref<string>("all");
const selectedHref = ref<string | null>(null);
const loading = ref(true);
const refreshing = ref(false);
const error = ref<string | null>(null);

let timer: ReturnType<typeof setInterval> | null = null;

async function load() {
  loading.value = true;
  error.value = null;
  try {
    addressbooks.value = await carddav.addressbooks();
    contacts.value = await carddav.contacts();
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    loading.value = false;
  }
  refresh(true);
}

async function refresh(silent = false) {
  if (refreshing.value) return;
  refreshing.value = true;
  if (!silent) error.value = null;
  try {
    addressbooks.value = await carddav.refresh();
    contacts.value = await carddav.contacts();
  } catch (e: any) {
    if (!silent) error.value = e?.message ?? String(e);
  } finally {
    refreshing.value = false;
  }
}

onMounted(() => {
  load();
  timer = setInterval(() => refresh(true), 5 * 60 * 1000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
});

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase();
  return contacts.value
    .filter((c) => activeAb.value === "all" || c.addressbookId === activeAb.value)
    .filter((c) => {
      if (!q) return true;
      return (
        c.fullName.toLowerCase().includes(q) ||
        c.emails.some((e) => e.value.toLowerCase().includes(q)) ||
        c.phones.some((p) => p.value.toLowerCase().includes(q)) ||
        (c.org ?? "").toLowerCase().includes(q)
      );
    })
    .sort((a, b) => a.fullName.localeCompare(b.fullName));
});

const selected = computed(() => contacts.value.find((c) => c.href === selectedHref.value) ?? null);

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  return (parts[0][0] + (parts.length > 1 ? parts[parts.length - 1][0] : "")).toUpperCase();
}

// ── editor ──────────────────────────────────────────────────────────────

// Right-click menu for a contact row.
const rowMenu = reactive({ open: false, x: 0, y: 0, contact: null as Contact | null });
function openRowMenu(e: MouseEvent, c: Contact) {
  rowMenu.x = e.clientX;
  rowMenu.y = e.clientY;
  rowMenu.contact = c;
  rowMenu.open = true;
}
function menuEdit() {
  const c = rowMenu.contact;
  rowMenu.open = false;
  if (c) openEdit(c);
}
function menuDelete() {
  const c = rowMenu.contact;
  rowMenu.open = false;
  if (c) remove(c);
}

const editorOpen = ref(false);
const editing = ref<Contact | null>(null);
const saving = ref(false);
const form = reactive({
  addressbookId: "",
  fullName: "",
  org: "",
  title: "",
  note: "",
  emails: [] as TypedValue[],
  phones: [] as TypedValue[],
});

function openNew() {
  editing.value = null;
  Object.assign(form, {
    addressbookId: addressbooks.value[0]?.id ?? "",
    fullName: "",
    org: "",
    title: "",
    note: "",
    emails: [{ label: "home", value: "" }],
    phones: [{ label: "cell", value: "" }],
  });
  editorOpen.value = true;
}

function openEdit(c: Contact) {
  editing.value = c;
  Object.assign(form, {
    addressbookId: c.addressbookId,
    fullName: c.fullName,
    org: c.org ?? "",
    title: c.title ?? "",
    note: c.note ?? "",
    emails: c.emails.length ? c.emails.map((e) => ({ ...e })) : [{ label: "home", value: "" }],
    phones: c.phones.length ? c.phones.map((p) => ({ ...p })) : [{ label: "cell", value: "" }],
  });
  editorOpen.value = true;
}

async function save() {
  if (!form.fullName.trim() || !form.addressbookId) return;
  saving.value = true;
  error.value = null;
  try {
    const input: ContactInput = {
      fullName: form.fullName.trim(),
      org: form.org.trim() || null,
      title: form.title.trim() || null,
      note: form.note.trim() || null,
      emails: form.emails.filter((e) => e.value.trim()),
      phones: form.phones.filter((p) => p.value.trim()),
    };
    const saved = await carddav.saveContact(
      form.addressbookId,
      input,
      editing.value?.href,
      editing.value?.etag,
    );
    contacts.value = contacts.value.filter((c) => c.href !== saved.href).concat(saved);
    selectedHref.value = saved.href;
    editorOpen.value = false;
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    saving.value = false;
  }
}

async function remove(c: Contact) {
  const ok = await ask(`Delete “${c.fullName}”?`, { title: "Delete contact", kind: "warning" });
  if (!ok) return;
  try {
    await carddav.deleteContact(c.addressbookId, c.href, c.etag);
    contacts.value = contacts.value.filter((x) => x.href !== c.href);
    if (selectedHref.value === c.href) selectedHref.value = null;
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Toolbar -->
    <header class="flex flex-wrap items-center gap-3 border-b border-line bg-surface px-5 py-3">
      <h1 class="flex items-center gap-2 text-base font-semibold text-ink">
        <Users class="h-5 w-5 text-accent" /> Contacts
      </h1>
      <select
        v-if="addressbooks.length > 1"
        v-model="activeAb"
        class="rounded-lg border border-line bg-canvas px-2 py-1 text-sm text-ink outline-none focus:border-accent"
      >
        <option value="all">All address books</option>
        <option v-for="ab in addressbooks" :key="ab.id" :value="ab.id">{{ ab.displayName }}</option>
      </select>
      <span class="flex-1" />
      <button
        class="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-ink-soft transition hover:bg-surface-alt"
        :disabled="refreshing"
        @click="refresh(false)"
      >
        <RefreshCw class="h-4 w-4" :class="refreshing ? 'animate-spin' : ''" /> Refresh
      </button>
      <button
        class="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white transition hover:opacity-90"
        @click="openNew"
      >
        <Plus class="h-4 w-4" /> New contact
      </button>
    </header>

    <p v-if="error" class="border-b border-line bg-negative/10 px-5 py-2 text-sm text-negative">
      {{ error }}
    </p>

    <div class="flex min-h-0 flex-1">
      <!-- List -->
      <div class="flex w-72 shrink-0 flex-col border-r border-line">
        <div class="border-b border-line p-2">
          <div class="flex items-center gap-2 rounded-lg bg-surface-alt px-2.5 py-1.5">
            <Search class="h-4 w-4 text-ink-soft" />
            <input
              v-model="query"
              placeholder="Search contacts"
              class="w-full bg-transparent text-sm text-ink outline-none placeholder:text-ink-soft"
            />
          </div>
        </div>
        <div class="min-h-0 flex-1 overflow-auto">
          <div v-if="loading" class="flex h-24 items-center justify-center text-ink-soft">
            <RefreshCw class="h-5 w-5 animate-spin" />
          </div>
          <p v-else-if="filtered.length === 0" class="px-4 py-8 text-center text-sm text-ink-soft">
            No contacts.
          </p>
          <button
            v-for="c in filtered"
            :key="c.href"
            class="flex w-full items-center gap-3 border-b border-line px-3 py-2 text-left transition hover:bg-surface-alt"
            :class="c.href === selectedHref ? 'bg-accent/10' : ''"
            @click="selectedHref = c.href"
            @contextmenu.prevent="openRowMenu($event, c)"
          >
            <span class="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-accent/15 text-xs font-semibold text-accent">
              {{ initials(c.fullName) }}
            </span>
            <span class="min-w-0 flex-1">
              <span class="block truncate text-sm text-ink">{{ c.fullName || "(no name)" }}</span>
              <span v-if="c.emails[0] || c.org" class="block truncate text-xs text-ink-soft">
                {{ c.emails[0]?.value || c.org }}
              </span>
            </span>
          </button>
        </div>
      </div>

      <!-- Detail -->
      <div class="min-h-0 flex-1 overflow-auto">
        <div v-if="!selected" class="flex h-full items-center justify-center text-sm text-ink-soft">
          Select a contact
        </div>
        <div v-else class="mx-auto max-w-xl px-6 py-8">
          <div class="flex items-center gap-4">
            <span class="grid h-16 w-16 shrink-0 place-items-center rounded-full bg-accent/15 text-xl font-semibold text-accent">
              {{ initials(selected.fullName) }}
            </span>
            <div class="min-w-0 flex-1">
              <h2 class="truncate text-xl font-semibold text-ink">{{ selected.fullName || "(no name)" }}</h2>
              <p v-if="selected.title || selected.org" class="truncate text-sm text-ink-soft">
                {{ [selected.title, selected.org].filter(Boolean).join(" · ") }}
              </p>
            </div>
            <button
              class="rounded-lg p-2 text-ink-soft hover:bg-surface-alt"
              title="Edit"
              @click="openEdit(selected)"
            >
              <Pencil class="h-4 w-4" />
            </button>
            <button
              class="rounded-lg p-2 text-ink-soft hover:bg-negative/10 hover:text-negative"
              title="Delete"
              @click="remove(selected)"
            >
              <Trash2 class="h-4 w-4" />
            </button>
          </div>

          <dl class="mt-6 space-y-4">
            <div v-if="selected.emails.length">
              <dt class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Email</dt>
              <a
                v-for="e in selected.emails"
                :key="e.value"
                :href="`mailto:${e.value}`"
                class="flex items-center gap-2 py-1 text-sm text-accent hover:underline"
              >
                <Mail class="h-4 w-4 text-ink-soft" /> {{ e.value }}
                <span v-if="e.label" class="text-xs text-ink-soft">· {{ e.label }}</span>
              </a>
            </div>
            <div v-if="selected.phones.length">
              <dt class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Phone</dt>
              <a
                v-for="p in selected.phones"
                :key="p.value"
                :href="`tel:${p.value}`"
                class="flex items-center gap-2 py-1 text-sm text-accent hover:underline"
              >
                <Phone class="h-4 w-4 text-ink-soft" /> {{ p.value }}
                <span v-if="p.label" class="text-xs text-ink-soft">· {{ p.label }}</span>
              </a>
            </div>
            <div v-if="selected.org">
              <dt class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Organization</dt>
              <p class="flex items-center gap-2 text-sm text-ink">
                <Building2 class="h-4 w-4 text-ink-soft" /> {{ selected.org }}
              </p>
            </div>
            <div v-if="selected.note">
              <dt class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Notes</dt>
              <p class="whitespace-pre-wrap text-sm text-ink">{{ selected.note }}</p>
            </div>
          </dl>
        </div>
      </div>
    </div>

    <!-- Contact right-click menu -->
    <template v-if="rowMenu.open">
      <div class="fixed inset-0 z-40" @click="rowMenu.open = false" @contextmenu.prevent="rowMenu.open = false" />
      <div
        class="fixed z-50 w-40 overflow-hidden rounded-lg border border-line bg-surface py-1 shadow-lg"
        :style="{ top: `${rowMenu.y}px`, left: `${rowMenu.x}px` }"
      >
        <button
          class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-surface-alt"
          @click="menuEdit"
        >
          <Pencil class="h-4 w-4 text-ink-soft" /> Edit
        </button>
        <button
          class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-negative hover:bg-negative/10"
          @click="menuDelete"
        >
          <Trash2 class="h-4 w-4" /> Delete
        </button>
      </div>
    </template>

    <!-- Editor modal -->
    <div
      v-if="editorOpen"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      @click.self="editorOpen = false"
    >
      <div class="flex max-h-[90vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl bg-surface shadow-xl">
        <header class="flex items-center justify-between border-b border-line px-5 py-3">
          <h2 class="text-sm font-semibold text-ink">{{ editing ? "Edit contact" : "New contact" }}</h2>
          <button class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="editorOpen = false">
            <X class="h-5 w-5" />
          </button>
        </header>

        <div class="flex flex-col gap-3 overflow-auto px-5 py-4">
          <input
            v-model="form.fullName"
            placeholder="Full name"
            class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          />
          <select
            v-if="addressbooks.length > 1"
            v-model="form.addressbookId"
            class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          >
            <option v-for="ab in addressbooks" :key="ab.id" :value="ab.id">{{ ab.displayName }}</option>
          </select>

          <!-- Emails -->
          <div>
            <p class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Email</p>
            <div v-for="(e, i) in form.emails" :key="`e${i}`" class="mb-1.5 flex items-center gap-2">
              <input
                v-model="e.label"
                placeholder="label"
                class="w-20 rounded-lg border border-line bg-canvas px-2 py-1.5 text-xs text-ink outline-none focus:border-accent"
              />
              <input
                v-model="e.value"
                placeholder="name@example.com"
                class="flex-1 rounded-lg border border-line bg-canvas px-3 py-1.5 text-sm text-ink outline-none focus:border-accent"
              />
              <button class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="form.emails.splice(i, 1)">
                <X class="h-4 w-4" />
              </button>
            </div>
            <button
              class="text-xs text-accent hover:underline"
              @click="form.emails.push({ label: 'home', value: '' })"
            >
              + Add email
            </button>
          </div>

          <!-- Phones -->
          <div>
            <p class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-soft">Phone</p>
            <div v-for="(p, i) in form.phones" :key="`p${i}`" class="mb-1.5 flex items-center gap-2">
              <input
                v-model="p.label"
                placeholder="label"
                class="w-20 rounded-lg border border-line bg-canvas px-2 py-1.5 text-xs text-ink outline-none focus:border-accent"
              />
              <input
                v-model="p.value"
                placeholder="+1 555 …"
                class="flex-1 rounded-lg border border-line bg-canvas px-3 py-1.5 text-sm text-ink outline-none focus:border-accent"
              />
              <button class="rounded p-1 text-ink-soft hover:bg-surface-alt" @click="form.phones.splice(i, 1)">
                <X class="h-4 w-4" />
              </button>
            </div>
            <button
              class="text-xs text-accent hover:underline"
              @click="form.phones.push({ label: 'cell', value: '' })"
            >
              + Add phone
            </button>
          </div>

          <div class="grid grid-cols-2 gap-2">
            <input
              v-model="form.org"
              placeholder="Organization"
              class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
            <input
              v-model="form.title"
              placeholder="Title"
              class="w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
          </div>
          <textarea
            v-model="form.note"
            placeholder="Notes"
            rows="3"
            class="w-full resize-none rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink outline-none focus:border-accent"
          />
        </div>

        <footer class="flex items-center justify-end border-t border-line px-5 py-3">
          <button
            class="rounded-lg px-3 py-1.5 text-sm text-ink-soft hover:bg-surface-alt"
            @click="editorOpen = false"
          >
            Cancel
          </button>
          <button
            class="ml-2 rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-white transition hover:opacity-90 disabled:opacity-50"
            :disabled="saving || !form.fullName.trim()"
            @click="save"
          >
            {{ saving ? "Saving…" : "Save" }}
          </button>
        </footer>
      </div>
    </div>
  </div>
</template>
