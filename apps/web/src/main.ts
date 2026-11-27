import { createApp } from "vue";
import { createPinia } from "pinia";
import { VueQueryPlugin } from "@tanstack/vue-query";
import { createRouter, createWebHistory } from "vue-router";
import App from "./App.vue";
import { ensureSession, onUnauthorized } from "./api/client";
import { notify } from "./lib/notify";
import "./styles.css";

// 高频页面静态导入：点导航不再发 chunk 请求，避免"资源更新后 chunk 404 → 页面不动"。
import Dashboard from "./views/Dashboard.vue";
import Jobs from "./views/Jobs.vue";
import JobDetail from "./views/JobDetail.vue";
import Sessions from "./views/Sessions.vue";
import Settings from "./views/Settings.vue";
import Editor from "./views/Editor.vue";
import Sql from "./views/Sql.vue";
import Login from "./views/Login.vue";
import Audit from "./views/Audit.vue";
import Clusters from "./views/Clusters.vue";

// 其余页面仍然懒加载（首屏更小）；加载失败由 router.onError 兜底。
const lazy = (p: () => Promise<unknown>) => p;

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/", name: "dashboard", component: Dashboard },
    { path: "/jobs", name: "jobs", component: Jobs },
    { path: "/jobs/:cluster/:ns/:name", name: "job", component: JobDetail, props: true },
    { path: "/clusters", name: "clusters", component: Clusters },
    {
      path: "/clusters/:id",
      name: "cluster",
      component: lazy(() => import("./views/ClusterDetail.vue")),
      props: true,
    },
    {
      path: "/savepoints",
      name: "savepoints",
      component: lazy(() => import("./views/Savepoints.vue")),
    },
    { path: "/metrics", name: "metrics", component: lazy(() => import("./views/Metrics.vue")) },
    { path: "/alerts", name: "alerts", component: lazy(() => import("./views/Alerts.vue")) },
    { path: "/submit", name: "submit", component: lazy(() => import("./views/Submit.vue")) },
    { path: "/editor/:cluster?/:ns?/:name?", name: "editor", component: Editor, props: true },
    { path: "/sql", name: "sql", component: Sql },
    { path: "/sessions", name: "sessions", component: Sessions },
    {
      path: "/templates",
      name: "templates",
      component: lazy(() => import("./views/Templates.vue")),
    },
    { path: "/presets", name: "presets", component: lazy(() => import("./views/Presets.vue")) },
    { path: "/deploy", name: "deploy", component: lazy(() => import("./views/Deploy.vue")) },
    {
      path: "/observability",
      name: "observability",
      component: lazy(() => import("./views/Observability.vue")),
    },
    { path: "/mcp", name: "mcp", component: lazy(() => import("./views/Mcp.vue")) },
    {
      path: "/namespaces",
      name: "namespaces",
      component: lazy(() => import("./views/Namespaces.vue")),
    },
    { path: "/users", name: "users", component: lazy(() => import("./views/Users.vue")) },
    { path: "/audit", name: "audit", component: Audit },
    { path: "/settings", name: "settings", component: Settings },
    {
      path: "/ui/:cluster/:ns/:name",
      name: "flinkui",
      component: lazy(() => import("./views/FlinkUi.vue")),
      props: true,
    },
    { path: "/login", name: "login", component: Login },
    { path: "/:pathMatch(.*)*", redirect: "/" },
  ],
});

onUnauthorized(() => {
  const cur = router.currentRoute.value;
  if (cur.name !== "login") void router.push({ name: "login", query: { redirect: cur.fullPath } });
});

router.beforeEach(async (to) => {
  if (to.name === "login") return true;
  try {
    await ensureSession();
    return true;
  } catch {
    return { name: "login", query: { redirect: to.fullPath } };
  }
});

/**
 * 导航失败兜底。
 * 最常见原因：部署更新后，浏览器里的旧 index.html 引用了已删除的 chunk hash，
 * 表现为"点了没反应，但 Network 里有请求（404）"。
 * 这里自动重载一次（用 sessionStorage 去重，防止死循环）。
 */
router.onError((err, to) => {
  const msg = String((err as Error)?.message ?? err);
  const isChunk =
    /Failed to fetch dynamically imported module|Importing a module script failed|Loading chunk|ChunkLoadError/i.test(
      msg,
    );
  if (!isChunk) {
    notify(`页面打开失败：${msg}`, "danger");
    return;
  }
  const key = "flinkops.chunk-reload";
  if (!sessionStorage.getItem(key)) {
    sessionStorage.setItem(key, "1");
    notify("前端资源已更新，正在重新加载…", "warn", 1500);
    setTimeout(() => location.assign(to.fullPath), 500);
  } else {
    notify("页面资源加载失败：请按 Ctrl+F5 强制刷新", "danger", 6000);
  }
});

router.afterEach(() => {
  // 导航成功说明资源是新的，清掉重载标记
  sessionStorage.removeItem("flinkops.chunk-reload");
});

// 当前 bundle 的构建时间（vite define 注入）。旧包与线上预期不一致时，
// 控制台这一行能让"你看到的是缓存还是新部署"一眼分清。
declare const __BUILD_TIME__: string;
console.info(`[flinkops] web build=${__BUILD_TIME__}`);

createApp(App)
  .use(createPinia())
  .use(VueQueryPlugin, {
    queryClientConfig: { defaultOptions: { queries: { staleTime: 10_000, retry: 1 } } },
  })
  .use(router)
  .mount("#app");
