import { createRouter, createWebHashHistory } from "vue-router";
import { useAuthStore } from "../stores/auth";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/overview" },
    {
      path: "/login",
      name: "login",
      component: () => import("../views/LoginView.vue"),
      meta: { public: true },
    },
    {
      path: "/overview",
      name: "overview",
      component: () => import("../views/OverviewView.vue"),
    },
    {
      path: "/files/:path(.*)?",
      name: "files",
      component: () => import("../views/FilesView.vue"),
    },
    {
      path: "/calendar",
      name: "calendar",
      component: () => import("../views/CalendarView.vue"),
    },
    {
      path: "/contacts",
      name: "contacts",
      component: () => import("../views/ContactsView.vue"),
    },
    // Synced folders now live on the Overview; keep the old path working.
    { path: "/sync", redirect: "/overview" },
    {
      path: "/trash",
      name: "trash",
      component: () => import("../views/TrashView.vue"),
    },
  ],
});

// Redirect to login when there is no active account.
router.beforeEach(async (to) => {
  const authStore = useAuthStore();
  if (authStore.account === null) {
    await authStore.refresh();
  }
  if (!to.meta.public && authStore.account === null) {
    return { name: "login" };
  }
  // /login doubles as "Add account", so it stays reachable while signed in.
  return true;
});

export default router;
