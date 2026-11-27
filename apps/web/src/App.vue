<script setup lang="ts">
import { computed, watch } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import { useQuery } from "@tanstack/vue-query";
import {
  LayoutDashboard,
  Workflow,
  Boxes,
  History,
  Gauge,
  BellRing,
  Rocket,
  Code2,
  TerminalSquare,
  Layers,
  Package,
  SlidersHorizontal,
  CloudUpload,
  Activity,
  FolderTree,
  Users,
  ScrollText,
  Settings as SettingsIcon,
  LogOut,
  Server,
  ShieldAlert,
} from "lucide-vue-next";
import { alerts, auth, jobs, setReadonlyMode } from "./api/client";
import { useCurrentCluster } from "./lib/currentCluster";
import { isRisk } from "./lib/jobFilters";
import { lang, t, toggleLang } from "./lib/i18n";
import Toasts from "./components/Toasts.vue";
import ConfirmHost from "./components/ConfirmHost.vue";

const route = useRoute();
const { data: me, error: meError } = useQuery({ queryKey: ["me"], queryFn: auth.me, retry: false });
// 只读模式：前端在请求层就拒绝写操作（后端仍会 403，这里只是别让用户白点）
watch(me, (v) => setReadonlyMode(!!(v as { readonly?: boolean } | undefined)?.readonly), {
  immediate: true,
});

// 顶栏：集群切换 + 健康摘要 + 告警（顶栏是固定的，任何时候都能看到自己在哪个集群）
const { current, effective, clusters } = useCurrentCluster();
const jobList = useQuery({
  queryKey: ["jobs"],
  queryFn: () => jobs.list(),
  refetchInterval: 60_000,
  retry: false,
});
const openAlerts = useQuery({
  queryKey: ["alerts", "open"],
  queryFn: () => alerts.events("OPEN"),
  refetchInterval: 60_000,
  retry: false,
});

const items = computed(() => jobList.data?.value?.items ?? []);
const running = computed(() => items.value.filter((j) => j.state === "RUNNING").length);
const attention = computed(() => items.value.filter(isRisk).length);
const openCount = computed(() => openAlerts.data?.value?.open ?? 0);
const p1Count = computed(
  () => (openAlerts.data?.value?.items ?? []).filter((e) => e.severity === "danger").length,
);

function pickCluster(e: Event) {
  current.value = (e.target as HTMLSelectElement).value;
}

// 语言切换要即时生效：这里是 computed，改 lang 会重新求值
const groups = computed(() => [
  {
    title: t("group.runtime"),
    items: [
      { to: "/", label: t("nav.overview"), icon: LayoutDashboard },
      { to: "/jobs", label: t("nav.jobs"), icon: Workflow },
      { to: "/clusters", label: t("nav.clusters"), icon: Boxes },
      { to: "/savepoints", label: t("nav.savepoints"), icon: History },
      { to: "/metrics", label: t("nav.metrics"), icon: Gauge },
      { to: "/alerts", label: t("nav.alerts"), icon: BellRing },
    ],
  },
  {
    title: t("group.delivery"),
    items: [
      { to: "/submit", label: t("nav.submit"), icon: Rocket },
      { to: "/editor", label: t("nav.editor"), icon: Code2 },
      { to: "/sql", label: t("nav.sql"), icon: TerminalSquare },
      { to: "/sessions", label: t("nav.sessions"), icon: Layers },
      { to: "/templates", label: t("nav.templates"), icon: Package },
    ],
  },
  {
    title: t("group.governance"),
    items: [
      { to: "/presets", label: t("nav.presets"), icon: SlidersHorizontal },
      { to: "/deploy", label: t("nav.deploy"), icon: CloudUpload },
      { to: "/observability", label: t("nav.observability"), icon: Activity },
      { to: "/mcp", label: t("nav.mcp"), icon: ShieldAlert },
      { to: "/namespaces", label: t("nav.namespaces"), icon: FolderTree },
      { to: "/users", label: t("nav.users"), icon: Users },
      { to: "/audit", label: t("nav.audit"), icon: ScrollText },
      { to: "/settings", label: t("nav.settings"), icon: SettingsIcon },
    ],
  },
]);

async function logout() {
  await auth.logout();
  location.assign("/login");
}
</script>

<template>
  <!-- 登录页：独立全屏布局，不套侧栏 -->
  <RouterView v-if="route.name === 'login'" />

  <div v-else class="grid min-h-screen" style="grid-template-columns: 232px minmax(0, 1fr)">
    <aside class="sticky top-0 flex h-screen flex-col border-r border-line-2 bg-surface">
      <div class="flex items-center gap-2 px-4 py-4">
        <span
          class="grid h-7 w-7 place-items-center rounded-lg bg-gradient-to-br from-accent to-[#ff9a8b] text-sm2 font-bold text-white"
          >F</span
        >
        <div class="leading-tight">
          <div class="text-sm2 font-semibold">FlinkOps</div>
          <div class="text-3xs uppercase tracking-widest text-fg-2">operator console</div>
        </div>
      </div>

      <nav class="flex-1 overflow-y-auto px-2 pb-2">
        <template v-for="g in groups" :key="g.title">
          <div class="px-2 pt-3 pb-1 text-3xs uppercase tracking-[0.13em] text-fg-2/70">
            {{ g.title }}
          </div>
          <RouterLink
            v-for="it in g.items"
            :key="it.to"
            :to="it.to"
            class="mb-0.5 flex items-center gap-2.5 rounded-lg px-2.5 py-[7px] text-xs text-fg-1 transition hover:bg-surface-2 hover:text-fg"
            active-class="bg-surface-2 text-fg"
          >
            <component :is="it.icon" :size="15" :stroke-width="1.8" class="opacity-85" />
            {{ it.label }}
          </RouterLink>
        </template>
      </nav>

      <div class="border-t border-line-2/70 p-2.5">
        <div v-if="me" class="rounded-lg bg-surface px-2.5 py-2 text-2xs">
          <div class="flex items-center gap-2">
            <span
              class="grid h-6 w-6 place-items-center rounded-full bg-gradient-to-br from-primary to-info text-3xs font-bold text-white"
            >
              {{ me.user.slice(0, 2).toUpperCase() }}
            </span>
            <div class="min-w-0">
              <div class="truncate text-fg-1">{{ me.user }}</div>
              <div class="truncate font-mono text-3xs text-fg-2">
                {{ me.clusters.length }} cluster · {{ me.prometheus ? "prom" : "rest-only" }}
              </div>
            </div>
            <button
              class="ml-auto text-fg-2 hover:text-danger"
              :title="t('shell.signOut')"
              @click="logout"
            >
              <LogOut :size="14" />
            </button>
          </div>
        </div>
        <div v-else-if="meError" class="rounded-lg bg-surface px-2.5 py-2 text-2xs">
          <div class="mb-0.5 text-fg-1">{{ t("shell.notSignedIn") }}</div>
          <RouterLink to="/login" class="text-primary hover:underline">{{
            t("shell.goSignIn")
          }}</RouterLink>
        </div>
      </div>
    </aside>

    <main class="min-w-0 px-6 py-0">
      <!-- 固定顶栏：集群永远可见（只有一个集群时也给出来，避免"我在哪"的疑问） -->
      <header
        class="sticky top-0 z-30 -mx-6 mb-4 flex flex-wrap items-center gap-2 border-b border-line-2 bg-app/90 px-6 py-2 backdrop-blur"
      >
        <label
          class="flex items-center gap-1.5 rounded-lg border border-line bg-surface px-2 py-1 text-2xs"
          :title="
            clusters.length > 1
              ? '切换集群：影响各页面的默认筛选'
              : '当前只有一个集群（接入更多集群后这里可切换）'
          "
        >
          <Server :size="13" :stroke-width="1.8" class="text-fg-2" />
          <span class="text-fg-2">{{ t("shell.cluster") }}</span>
          <select
            :value="effective"
            class="cursor-pointer bg-transparent font-mono text-2xs text-fg-1 outline-none disabled:cursor-default"
            :disabled="clusters.length <= 1"
            @change="pickCluster"
          >
            <option v-for="c in clusters" :key="c" :value="c">{{ c }}</option>
            <!-- 首屏 /api/me 还没回来时显示"加载中"，否则会短暂显示"未接入"吓人一跳 -->
            <option v-if="!clusters.length" value="">{{ me ? "未接入" : "加载中…" }}</option>
          </select>
          <span v-if="clusters.length > 1" class="font-mono text-3xs text-fg-2"
            >/ {{ clusters.length }}</span
          >
        </label>

        <div
          class="hidden items-center gap-2 rounded-lg border border-line bg-surface px-2 py-1 text-2xs md:flex"
          :title="`${running} 个 RUNNING · ${attention} 个需要关注`"
        >
          <span
            class="inline-block h-1.5 w-1.5 rounded-full"
            :class="attention ? 'bg-warn' : 'bg-ok'"
          ></span>
          <span class="font-mono">{{ running }}</span><span class="text-fg-2">{{
            t("top.running")
          }}</span>
          <span v-if="attention" class="font-mono text-warn"
            >· {{ attention }} {{ t("top.attention") }}</span
          >
        </div>

        <span
          v-if="me?.readonly"
          class="flex items-center gap-1 rounded-lg border border-warn/30 bg-warn/10 px-2 py-1 text-2xs text-warn"
          :title="t('shell.readonlyHint')"
        >
          <ShieldAlert :size="12" :stroke-width="1.8" />{{ t("shell.readonly") }}
        </span>

        <div class="ml-auto flex flex-wrap items-center gap-2">
          <RouterLink
            to="/alerts"
            class="flex items-center gap-1.5 rounded-lg border px-2 py-1 text-2xs transition"
            :class="
              openCount
                ? 'border-danger/30 bg-danger/10 text-danger hover:bg-danger/15'
                : 'border-line bg-surface text-fg-2 hover:text-fg'
            "
            :title="`${openCount} 条待处理告警（P1 ${p1Count}）`"
          >
            <BellRing :size="13" :stroke-width="1.8" />
            <span>{{ t("top.alerts") }}</span>
            <span
              v-if="openCount"
              class="rounded-full bg-danger px-1.5 font-mono text-3xs text-white"
              >{{ openCount }}</span
            >
          </RouterLink>
          <RouterLink
            to="/submit"
            class="flex items-center gap-1.5 rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white hover:opacity-90"
          >
            <Rocket :size="13" :stroke-width="1.8" />{{ t("top.submit") }}
          </RouterLink>
          <button
            class="rounded-lg border border-line bg-surface px-2 py-1 font-mono text-2xs text-fg-2 hover:text-fg"
            :title="t('top.lang')"
            @click="toggleLang"
          >
            {{ lang === "zh" ? "EN" : "中" }}
          </button>
          <RouterLink
            to="/settings"
            class="rounded-lg border border-line bg-surface px-2 py-1 text-2xs text-fg-2 hover:text-fg"
            :title="t('shell.settings')"
          >
            <SettingsIcon :size="13" :stroke-width="1.8" />
          </RouterLink>
        </div>
      </header>

      <div class="pb-5"><RouterView /></div>
    </main>
  </div>

  <Toasts />
  <ConfirmHost />
</template>
