<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { RouterLink, useRoute, useRouter } from "vue-router";
import PageHead from "../components/PageHead.vue";
import Tabs from "../components/Tabs.vue";
import DataTable from "../components/DataTable.vue";
import Badge from "../components/Badge.vue";
import Modal from "../components/Modal.vue";
import { jobs, prom } from "../api/client";
import { useCurrentCluster } from "../lib/currentCluster";
import { isRisk, isSuspended } from "../lib/jobFilters";
import { useWritable } from "../lib/session";
import { download, notify } from "../lib/notify";
import type { JobSummary } from "../api/client";
import { t } from "../lib/i18n";

const qc = useQueryClient();
const router = useRouter();
// 筛选状态与 URL 双向绑定：排障时把链接发给同事要能看到同一屏
const route = useRoute();
const tab = ref(String(route.query.tab ?? "all"));
const kw = ref(String(route.query.q ?? ""));
const ns = ref(String(route.query.ns ?? ""));
// 排序 / 页码同样进 URL：排障时把链接发出去，对方看到的是同一屏
const sortKey = ref(String(route.query.sort ?? ""));
const sortDir = ref<"asc" | "desc">(route.query.dir === "desc" ? "desc" : "asc");
const page = ref(Math.max(1, Number(route.query.page ?? 1) || 1));
const { effective: curCluster } = useCurrentCluster();
const picked = ref<string[]>([]);
const { readonly } = useWritable();
const busy = ref(false);
const menu = ref("");
// 点空白处 / Esc 收起行操作菜单：否则它会一直挂着，看着像是错位
function closeMenu() {
  menu.value = "";
}
onMounted(() => {
  window.addEventListener("click", closeMenu);
  window.addEventListener("keydown", onMenuKey);
});
onBeforeUnmount(() => {
  window.removeEventListener("click", closeMenu);
  window.removeEventListener("keydown", onMenuKey);
});
function onMenuKey(e: KeyboardEvent) {
  if (e.key === "Escape") menu.value = "";
}

const list = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });
const sync = useMutation({
  mutationFn: () => jobs.list({ refresh: true }),
  onSuccess: () => qc.invalidateQueries({ queryKey: ["jobs"] }),
});

const items = computed(() => list.data?.value?.items ?? []);
const listError = computed(() =>
  list.isError.value ? ((list.error.value as Error | null)?.message ?? t("jobs.loadFailed")) : "",
);

// 索引新鲜度：列表来自 SQL 索引，详情来自实时 CRD。滞后超过 5 分钟就显式提示，
// 避免"列表 RUNNING / 详情 RECONCILING"被当成 bug（实测旧构建可滞后数小时）。
const indexAge = computed(() => {
  const v = Number((list.data?.value as { indexAgeSecs?: number } | undefined)?.indexAgeSecs ?? 0);
  return Number.isFinite(v) ? v : 0;
});
const staleIndex = computed(() => (indexAge.value > 300 ? Math.round(indexAge.value) : 0));
const indexAsOf = computed(() => {
  const a = (list.data?.value as { asOf?: string } | undefined)?.asOf;
  if (!a) return t("common.unknown");
  try {
    return new Date(a).toLocaleTimeString("zh-CN", { hour12: false });
  } catch {
    return a;
  }
});

// 筛选/tab/命名空间一变，选择集合必须清空：
// 否则"批量 Savepoint"会把已经看不见的作业一起打快照（真实事故）。
watch([tab, kw, ns, curCluster], () => {
  picked.value = [];
});

watch([tab, kw, ns, sortKey, sortDir, page], () => {
  void router.replace({
    query: {
      ...route.query,
      tab: tab.value === "all" ? undefined : tab.value,
      q: kw.value || undefined,
      ns: ns.value || undefined,
      sort: sortKey.value || undefined,
      dir: sortKey.value ? sortDir.value : undefined,
      page: page.value > 1 ? String(page.value) : undefined,
    },
  });
});
const namespaces = computed(() => [...new Set(items.value.map((j) => j.namespace))].sort());
const key = (j: JobSummary) => `${j.cluster}/${j.namespace}/${j.name}`;

// 吞吐 / 背压：Prometheus 按 job_id 聚合，映射回 Deployment 名
const rt = useQuery({
  queryKey: ["jobs", "runtime-metrics"],
  queryFn: async () => {
    const [thr, bp] = await Promise.all([
      // 与大盘同口径：只算 Source 算子的出口速率（全算子求和会把链式算子重复计入）
      prom.query(
        'sum by (job_id) (flink_taskmanager_job_task_operator_numRecordsOutPerSecond{task_name=~"Source.*"})',
      ),
      prom.query("sum by (job_id) (flink_taskmanager_job_task_backPressuredTimeMsPerSecond)"),
    ]);
    const m = (r: typeof thr) => {
      const out = new Map<string, number>();
      for (const s of r?.data?.result ?? [])
        out.set(s.metric.job_id ?? "", Number(s.value?.[1] ?? 0));
      return out;
    };
    return { thr: m(thr), bp: m(bp) };
  },
  refetchInterval: 60_000,
  retry: false,
});

const view = ref<"table" | "card">("table");

const runtime = (startedAt?: string) => {
  if (!startedAt) return "—";
  const s = Math.max(0, (Date.now() - new Date(startedAt).getTime()) / 1000);
  if (!Number.isFinite(s)) return "—";
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${(s / 3600).toFixed(1)}h`;
  return `${(s / 86400).toFixed(1)}d`;
};
const bpText = (ms: number) => (ms > 500 ? "HIGH" : ms > 100 ? "MED" : ms > 0 ? "LOW" : "—");
// DataTable 的 row 是 Record<string, unknown>，模板里统一走这两个窄化函数
const fmtThr = (v: unknown) => {
  const n = Number(v);
  if (!Number.isFinite(n)) return "—";
  return n >= 1000 ? `${(n / 1000).toFixed(1)}K` : String(Math.round(n));
};
const bpOf = (v: unknown) => Number(v) || 0;
// DataTable 的 slot 只保证 Record<string, unknown>；这里是唯一需要窄化的边界
const asJob = (r: Record<string, unknown>) => r as unknown as JobSummary;
const bpTone = (v: unknown) => {
  const t = bpText(bpOf(v));
  return t === "HIGH"
    ? "bg-danger/10 text-danger"
    : t === "MED"
      ? "bg-warn/10 text-warn"
      : t === "LOW"
        ? "bg-ok/10 text-ok"
        : "text-fg-2";
};
const bpLabel = (v: unknown) => bpText(bpOf(v));

// 状态语义按"用户想找什么"定义，不再用 state !== RUNNING 这种近似：
// 已挂起 = SUSPENDED；需要关注 = 真失败（FAILED / health=red）；
// FINISHED/CREATED 是正常过程态，不进"需要关注"。
// 判定逻辑在 lib/jobFilters，顶栏与列表共用同一套口径

const rows = computed(() =>
  items.value
    .filter((j) =>
      tab.value === "all"
        ? true
        : tab.value === "risk"
          ? isRisk(j)
          : tab.value === "stopped"
            ? isSuspended(j)
            : j.kind === "FlinkSessionJob",
    )
    // 全局集群切换器影响默认范围；页内没有单独的集群下拉，靠它切
    .filter((j) => (curCluster.value ? j.cluster === curCluster.value : true))
    .filter((j) => (ns.value ? j.namespace === ns.value : true))
    .filter((j) =>
      kw.value ? `${j.namespace}/${j.name}`.toLowerCase().includes(kw.value.toLowerCase()) : true,
    )
    .map((j) => ({
      ...j,
      // 唯一键：同名作业可能分布在多个 namespace / cluster
      key: key(j),
      slotText: `${j.taskManagerReplicas ?? "—"} × ${j.slotsPerTm ?? "—"}`,
      thr: j.jobId ? (rt.data?.value?.thr.get(j.jobId) ?? NaN) : NaN,
      bp: j.jobId ? (rt.data?.value?.bp.get(j.jobId) ?? 0) : 0,
      runtime: runtime(j.startedAt),
    })),
);

const tabs = computed(() => [
  { key: "all", label: t("jobs.tab.all"), badge: items.value.length },
  { key: "risk", label: t("jobs.tab.risk"), badge: items.value.filter(isRisk).length },
  { key: "stopped", label: t("jobs.tab.stopped"), badge: items.value.filter(isSuspended).length },
  {
    key: "session",
    label: "SessionJob",
    badge: items.value.filter((j) => j.kind === "FlinkSessionJob").length,
  },
]);

// ---- Savepoint 弹窗（带路径输入）
const spOpen = ref(false);
const spPath = ref("");
const spTarget = ref<JobSummary | null>(null);
function openSavepoint(j: JobSummary) {
  spTarget.value = j;
  spPath.value = "";
  spOpen.value = true;
  menu.value = "";
}
async function doSavepoint() {
  const j = spTarget.value;
  if (!j) return;
  const path = spPath.value.trim();
  // 覆盖提示：Flink 的 savepoint 目录复用会覆盖上一份，很多集群 default 路径就是作业级目录
  if (
    path &&
    !(await confirmDialog({
      message: t("jobs.savepointOverwrite", { path }),
      danger: true,
    }))
  )
    return;
  busy.value = true;
  try {
    const r = await jobs.savepoint(j.cluster, j.namespace, j.name, path || undefined);
    const st = (r as { state?: string }).state;
    notify(
      t("jobs.savepointSubmitted", { id: r.snapshot ?? "", state: st ? ` (${st})` : "" }),
      "ok",
      5000,
    );
    spOpen.value = false;
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
  }
}

// ---- 单作业操作
async function setState(j: JobSummary, running: boolean) {
  busy.value = true;
  try {
    await jobs.setState(j.cluster, j.namespace, j.name, running);
    notify(
      t("jobs.stateChanged", {
        name: j.name,
        action: running ? t("jobs.resume") : t("jobs.suspend"),
      }),
      "ok",
    );
    await qc.invalidateQueries({ queryKey: ["jobs"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
    menu.value = "";
  }
}
// 删除用页面内 Modal（原来用 window.prompt：不可主题化、部分 WebView 会屏蔽、
// 也没法在弹窗里展示"将一并删除什么"）。
const delOpen = ref(false);
const delText = ref("");
const delTarget = ref<JobSummary | null>(null);
function removeJob(j: JobSummary) {
  menu.value = "";
  delTarget.value = j;
  delText.value = "";
  delOpen.value = true;
}
async function confirmDelete() {
  const j = delTarget.value;
  if (!j) return;
  if (delText.value.trim() !== j.name) {
    notify(t("jobs.nameMismatch"), "warn");
    return;
  }
  busy.value = true;
  try {
    await jobs.remove(j.cluster, j.namespace, j.name);
    notify(t("jobs.deleted", { name: j.name }), "ok");
    delOpen.value = false;
    await qc.invalidateQueries({ queryKey: ["jobs"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
  }
}

async function restart(j: JobSummary) {
  menu.value = "";
  if (!(await confirmDialog({ message: t("jobs.restartConfirm", { name: j.name }) }))) return;
  busy.value = true;
  try {
    const r = await jobs.restart(j.cluster, j.namespace, j.name);
    notify(t("jobs.restarted", { nonce: r.restartNonce ?? "" }), "ok");
    await qc.invalidateQueries({ queryKey: ["jobs"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
  }
}

// ---- 批量
async function batchSavepoint() {
  if (!picked.value.length) {
    notify(t("jobs.pickFirst"), "warn");
    return;
  }
  // 只对"当前筛选下还能看见"的作业生效：勾选后切了 tab/筛选，残留的 key
  // 会把用户看不见的作业一起打快照（真实事故）。跳过的数量要显式告知。
  const visible = new Set(rows.value.map((r) => key(r)));
  const targets = picked.value.filter((k) => visible.has(k));
  const hidden = picked.value.length - targets.length;
  if (!targets.length) {
    notify(t("jobs.selectionOutOfView"), "warn");
    return;
  }
  if (
    !(await confirmDialog({
      message:
        t("jobs.batchConfirm", { n: targets.length }) +
        (hidden ? t("jobs.batchHidden", { n: hidden }) : ""),
    }))
  )
    return;
  busy.value = true;
  let ok = 0;
  const seen = new Set<string>();
  for (const k of targets) {
    // picked 里存的是 cluster/ns/name 三元组；只按 name 查会在多命名空间/多集群
    // 出现同名作业时命中错误的那个（对错作业打 savepoint 是真实事故）
    const j = items.value.find((x) => key(x) === k);
    if (!j || j.kind !== "FlinkDeployment" || seen.has(k)) continue;
    seen.add(k);
    try {
      await jobs.savepoint(j.cluster, j.namespace, j.name);
      ok += 1;
    } catch {
      /* 单个失败继续 */
    }
  }
  busy.value = false;
  notify(
    t("jobs.batchDone", { ok, total: targets.length }) +
      (hidden ? t("jobs.batchSkipped", { n: hidden }) : ""),
    ok ? "ok" : "danger",
  );
}

function exportCsv() {
  const head = "cluster,namespace,name,kind,state,health,parallelism,slots,flinkVersion";
  const esc = (v: unknown) => `"${String(v ?? "").replace(/"/g, '""')}"`;
  const body = rows.value.map((j) =>
    [
      j.cluster,
      j.namespace,
      j.name,
      j.kind,
      j.state,
      j.health,
      j.parallelism ?? "",
      `${j.taskManagerReplicas ?? ""}x${j.slotsPerTm ?? ""}`,
      j.flinkVersion ?? "",
    ]
      .map(esc)
      .join(","),
  );
  download(`flinkops-jobs-${Date.now()}.csv`, [head, ...body].join("\n"), "text/csv;charset=utf-8");
  notify(t("jobs.exported", { n: rows.value.length }), "ok");
}

const columns = computed(() => [
  { key: "name", label: t("jobs.col.name") },
  { key: "kind", label: t("jobs.col.kind") },
  { key: "state", label: t("jobs.col.state") },
  { key: "thr", label: t("jobs.col.throughput"), align: "right" as const },
  { key: "bp", label: t("jobs.col.backpressure"), align: "right" as const },
  { key: "runtime", label: t("jobs.col.runtime"), align: "right" as const },
  { key: "parallelism", label: t("jobs.col.parallelism"), align: "right" as const },
  { key: "slotText", label: t("jobs.col.slots"), align: "right" as const, sortable: false },
  { key: "image", label: t("jobs.col.image"), align: "left" as const, sortable: false },
  { key: "res", label: t("jobs.col.resources"), align: "left" as const, sortable: false },
  { key: "par", label: t("jobs.col.parallelismActual"), align: "right" as const, sortable: false },
  { key: "flinkVersion", label: "Flink", align: "right" as const },
  { key: "op", label: t("jobs.col.actions"), align: "right" as const, sortable: false },
]);
</script>

<template>
  <PageHead :title="t('page.jobs')" :sub="t('jobs.subCount', { n: items.length })">
    <p
      v-if="staleIndex"
      class="mt-1 rounded-lg border border-warn/40 bg-warn/10 px-2.5 py-1 text-2xs text-warn"
    >
      {{ t("jobs.staleIndex", { sec: staleIndex, asOf: indexAsOf }) }}
    </p>
    <template #actions>
      <button
        class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 text-xs hover:border-primary/50"
        @click="exportCsv"
      >
        {{ t("jobs.export") }}
      </button>
      <button
        class="rounded-lg bg-primary px-2.5 py-1 text-xs font-medium text-white"
        @click="batchSavepoint"
        :disabled="readonly"
      >
        {{ t("jobs.batchSavepoint") }}{{ picked.length ? ` (${picked.length})` : "" }}
      </button>
      <button
        class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 text-xs hover:border-primary/50"
        :disabled="sync.isPending.value"
        @click="sync.mutate()"
      >
        {{ sync.isPending.value ? t("jobs.syncing") : t("jobs.refresh") }}
      </button>
    </template>
  </PageHead>

  <div class="mb-2 flex flex-wrap items-center gap-2 text-xs">
    <select v-model="ns" class="rounded-lg border border-line bg-surface-2 px-2 py-1">
      <option value="">{{ t("jobs.allNamespaces") }}</option>
      <option v-for="n in namespaces" :key="n" :value="n">{{ n }}</option>
    </select>
    <input
      v-model="kw"
      :placeholder="t('jobs.searchPlaceholder')"
      class="w-40 rounded-lg border border-line bg-surface-2 px-2 py-1"
    />
    <div class="ml-auto inline-flex overflow-hidden rounded-lg border border-line text-2xs">
      <button
        class="px-2.5 py-1"
        :class="view === 'table' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
        @click="view = 'table'"
      >
        {{ t("jobs.viewTable") }}
      </button>
      <button
        class="px-2.5 py-1"
        :class="view === 'card' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
        @click="view = 'card'"
      >
        {{ t("jobs.viewCards") }}
      </button>
    </div>
    <span v-if="picked.length" class="text-fg-2">
      {{ t("jobs.selected", { n: picked.length }) }}
    </span>
  </div>

  <Tabs v-model="tab" :tabs="tabs" />

  <DataTable
    v-if="view === 'table'"
    :columns="columns"
    :rows="rows"
    :page-size="20"
    :error="listError"
    :empty="t('jobs.empty')"
    row-key="key"
    selectable
    v-model:selected="picked"
    v-model:sort="sortKey"
    v-model:dir="sortDir"
    v-model:page="page"
  >
    <template #cell-name="{ row }">
      <RouterLink
        :to="`/jobs/${row.cluster}/${row.namespace}/${row.name}`"
        class="font-medium hover:text-primary"
        >{{ row.name }}</RouterLink
      >
      <div class="font-mono text-3xs text-fg-2">{{ row.cluster }} / {{ row.namespace }}</div>
    </template>
    <template #cell-kind="{ row }">{{
      row.kind === "FlinkSessionJob" ? "SessionJob" : "Deployment"
    }}</template>
    <template #cell-state="{ row }">
      <Badge :tone="row.health === 'green' ? 'ok' : row.health === 'red' ? 'danger' : 'warn'" dot>{{
        row.state
      }}</Badge>
    </template>
    <template #cell-res="{ row }">
      <span class="block whitespace-nowrap font-mono text-3xs text-fg-2">
        {{ row.jobManagerCpu ?? "—" }}/{{ row.jobManagerMemory ?? "—" }} ×
        {{ row.taskManagerReplicas ?? "—" }}·{{ row.taskManagerCpu ?? "—" }}/{{
          row.taskManagerMemory ?? "—"
        }}
      </span>
    </template>
    <template #cell-par="{ row }">
      <span
        class="font-mono text-2xs"
        :title="row.parallelismActual ? t('jobs.actualTip') : t('jobs.actualUnknown')"
      >
        {{ row.parallelism ?? "—"
        }}{{
          row.parallelismActual && row.parallelismActual !== row.parallelism
            ? t("jobs.actualSuffix", { n: Number(row.parallelismActual) })
            : ""
        }}
      </span>
    </template>
    <template #cell-image="{ row }">
      <span
        class="block max-w-[220px] truncate font-mono text-3xs text-fg-2"
        :title="String(row.image ?? '')"
      >
        {{ row.image ?? "—" }}
      </span>
    </template>
    <template #cell-thr="{ row }">
      <span class="font-mono text-2xs">{{ fmtThr(row.thr) }}</span>
    </template>
    <template #cell-bp="{ row }">
      <span class="rounded-full px-1.5 py-0.5 text-3xs" :class="bpTone(row.bp)">{{
        bpLabel(row.bp)
      }}</span>
    </template>
    <template #cell-runtime="{ row }"
      ><span class="font-mono text-2xs text-fg-2">{{ row.runtime }}</span></template
    >
    <template #cell-op="{ row }">
      <div class="flex items-center justify-end gap-1.5 whitespace-nowrap">
        <button
          v-if="row.state === 'RUNNING'"
          :disabled="busy || readonly"
          class="text-2xs text-warn hover:underline"
          @click="setState(asJob(row), false)"
        >
          {{ t("jobs.suspend") }}
        </button>
        <button
          v-else
          :disabled="busy || readonly"
          class="text-2xs text-ok hover:underline"
          @click="setState(asJob(row), true)"
        >
          {{ t("jobs.resume") }}
        </button>
        <button
          :disabled="busy || readonly"
          class="text-2xs text-fg-1 hover:underline"
          @click="restart(asJob(row))"
        >
          {{ t("jobs.restart") }}
        </button>
        <button
          :disabled="busy || readonly"
          class="text-2xs text-fg-1 hover:underline"
          @click="openSavepoint(asJob(row))"
        >
          Savepoint
        </button>
        <a
          :href="`/ui/${row.cluster}/${row.namespace}/${row.name}`"
          target="_blank"
          class="text-2xs text-primary hover:underline"
          >UI</a
        >

        <div class="relative">
          <button
            class="cursor-pointer rounded px-1.5 text-sm2 text-fg-2 hover:bg-hover hover:text-fg"
            :aria-expanded="menu === key(asJob(row))"
            @click.stop="menu = menu === key(asJob(row)) ? '' : key(asJob(row))"
          >
            ···
          </button>
          <div
            v-if="menu === key(asJob(row))"
            class="absolute top-full right-0 z-30 mt-1 min-w-[11rem] overflow-hidden rounded-lg border border-line-2 bg-surface-2 py-1 text-left shadow-xl"
            @click.stop
          >
            <button
              class="block w-full px-3 py-1.5 text-left text-xs hover:bg-hover"
              @click="
                menu = '';
                router.push(`/jobs/${row.cluster}/${row.namespace}/${row.name}`);
              "
            >
              {{ t("jobs.menuDetail") }}
            </button>
            <button
              class="block w-full px-3 py-1.5 text-left text-xs hover:bg-hover"
              @click="
                menu = '';
                router.push(`/editor/${row.cluster}/${row.namespace}/${row.name}`);
              "
            >
              {{ t("jobs.menuEdit") }}
            </button>
            <button
              class="block w-full px-3 py-1.5 text-left text-xs hover:bg-hover"
              @click="
                menu = '';
                router.push(`/jobs/${row.cluster}/${row.namespace}/${row.name}`);
              "
            >
              {{ t("jobs.menuLogs") }}
            </button>
            <button
              :disabled="readonly"
              class="block w-full px-3 py-1.5 text-left text-xs hover:bg-hover disabled:opacity-40"
              @click="openSavepoint(asJob(row))"
            >
              {{ t("jobs.menuSavepoint") }}
            </button>
            <div class="my-1 h-px bg-line-2"></div>
            <button
              :disabled="readonly"
              class="block w-full px-3 py-1.5 text-left text-xs text-danger hover:bg-hover disabled:opacity-40"
              @click="removeJob(asJob(row))"
            >
              {{ t("jobs.menuDelete") }}
            </button>
          </div>
        </div>
      </div>
    </template>
  </DataTable>

  <!-- 卡片视图 -->
  <div
    v-else
    class="grid gap-2"
    style="grid-template-columns: repeat(auto-fill, minmax(260px, 1fr))"
  >
    <div
      v-for="row in rows"
      :key="key(asJob(row))"
      class="cursor-pointer rounded-xl border border-line-2 bg-surface px-3 py-2.5 transition hover:border-primary/40"
      @click="router.push(`/jobs/${row.cluster}/${row.namespace}/${row.name}`)"
    >
      <div class="flex items-center gap-2">
        <span class="truncate font-medium">{{ row.name }}</span>
        <Badge
          class="ml-auto"
          :tone="row.health === 'green' ? 'ok' : row.health === 'red' ? 'danger' : 'warn'"
          dot
          >{{ row.state }}</Badge
        >
      </div>
      <div class="mt-0.5 font-mono text-3xs text-fg-2">{{ row.cluster }} / {{ row.namespace }}</div>
      <div class="mt-2 grid grid-cols-2 gap-1 text-2xs">
        <div class="flex justify-between">
          <span class="text-fg-2">{{ t("jobs.col.throughput") }}</span><span class="font-mono">{{
            Number.isFinite(row.thr)
              ? row.thr >= 1000
                ? (row.thr / 1000).toFixed(1) + "K"
                : String(Math.round(row.thr))
              : "—"
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">{{ t("jobs.col.backpressure") }}</span><span class="font-mono">{{
            bpText(row.bp)
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">{{ t("jobs.col.parallelism") }}</span><span class="font-mono">{{
            row.parallelism ?? "—"
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">{{ t("jobs.running") }}</span><span class="font-mono">{{
            row.runtime
          }}</span>
        </div>
      </div>
    </div>
    <div
      v-if="!rows.length"
      class="col-span-full rounded-xl border border-line-2 bg-surface px-4 py-8 text-center text-xs text-fg-2"
    >
      {{ t("jobs.empty") }}
    </div>
  </div>

  <div class="mt-2 flex items-center gap-3 text-2xs text-fg-2">
    <label class="flex items-center gap-1.5">
      <input
        type="checkbox"
        :checked="picked.length > 0 && picked.length === rows.length"
        class="accent-[var(--color-primary)]"
        @change="picked = ($event.target as HTMLInputElement).checked ? rows.map(key) : []"
      />
      {{ t("jobs.selectPage") }}
    </label>
    <span>{{ t("jobs.footer", { n: rows.length }) }}</span>
  </div>

  <Modal
    v-model="spOpen"
    :title="t('jobs.savepointTitle')"
    :sub="spTarget ? `${spTarget.namespace} / ${spTarget.name}` : ''"
  >
    <label class="flex flex-col gap-1 text-xs">
      <span class="text-fg-2">{{ t("jobs.savepointPath") }}</span>
      <input
        v-model="spPath"
        placeholder="s3://flink-state/savepoints/orders"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      />
    </label>
    <p class="mt-2 text-2xs text-fg-2">
      {{ t("jobs.savepointNote", { kind: "FlinkStateSnapshot" }) }}
    </p>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="spOpen = false"
      >
        {{ t("common.cancel") }}
      </button>
      <button
        :disabled="busy"
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
        @click="doSavepoint"
      >
        {{ t("jobs.trigger") }}
      </button>
    </template>
  </Modal>

  <Modal
    v-model="delOpen"
    persistent
    :title="t('jobs.menuDelete')"
    :sub="delTarget ? `${delTarget.cluster}/${delTarget.namespace}/${delTarget.name}` : ''"
  >
    <p class="text-xs text-danger">
      {{ t("jobs.deleteWarn") }}
    </p>
    <label class="mt-3 flex flex-col gap-1 text-xs">
      <span class="text-fg-2"
        >{{ t("jobs.deleteConfirmLabel")
        }}<code class="font-mono">{{ delTarget?.name }}</code></span
      >
      <input
        v-model="delText"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        :placeholder="delTarget?.name"
      />
    </label>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="delOpen = false"
      >
        {{ t("common.cancel") }}
      </button>
      <button
        :disabled="busy || delText.trim() !== (delTarget?.name ?? '')"
        class="rounded-lg bg-danger px-3 py-1 text-xs font-medium text-white disabled:opacity-40"
        @click="confirmDelete"
      >
        {{ t("common.delete") }}
      </button>
    </template>
  </Modal>
</template>
