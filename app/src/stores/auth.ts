import { defineStore } from "pinia";
import { ref } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";
import { auth } from "../api";
import type { Account, ServerKind } from "../api/types";

export const useAuthStore = defineStore("auth", () => {
  /** The account currently being browsed (Files/Overview/Trash). */
  const account = ref<Account | null>(null);
  /** All connected accounts. */
  const accounts = ref<Account[]>([]);
  const status = ref<"idle" | "waiting" | "polling">("idle");
  const error = ref<string | null>(null);

  let pollTimer: number | null = null;

  function stopPolling() {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    status.value = "idle";
  }

  /** Load accounts + active account restored by the backend at startup. */
  async function refresh() {
    try {
      accounts.value = await auth.listAccounts();
      account.value = await auth.activeAccount();
    } catch {
      accounts.value = [];
      account.value = null;
    }
  }

  /** Begin Nextcloud Login Flow v2: open the browser and poll until approval. */
  async function login(serverUrl: string) {
    error.value = null;
    stopPolling();
    try {
      const init = await auth.startLogin(serverUrl);
      await openUrl(init.loginUrl);
      status.value = "polling";

      pollTimer = window.setInterval(async () => {
        try {
          const acc = await auth.pollLogin(init.pollEndpoint, init.pollToken);
          if (acc) {
            stopPolling();
            await refresh();
          }
        } catch (e) {
          error.value = errMessage(e);
          stopPolling();
        }
      }, 2000);
    } catch (e) {
      error.value = errMessage(e);
      status.value = "idle";
    }
  }

  /** Connect a Nextcloud or ownCloud account with an app password. */
  async function addManual(
    serverUrl: string,
    username: string,
    password: string,
    kind: ServerKind,
  ) {
    error.value = null;
    await auth.addManual(serverUrl, username, password, kind);
    await refresh();
  }

  async function setActive(accountId: string) {
    await auth.setActiveAccount(accountId);
    await refresh();
  }

  async function removeAccount(accountId: string) {
    await auth.removeAccount(accountId);
    await refresh();
  }

  /** Top-bar account menu "Disconnect": drops the account currently browsed. */
  async function logout() {
    if (account.value) await removeAccount(account.value.id);
  }

  return {
    account,
    accounts,
    status,
    error,
    refresh,
    login,
    addManual,
    setActive,
    removeAccount,
    logout,
    stopPolling,
  };
});

function errMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}
