<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import LineChart from "../components/charts/LineChart.vue";
import Sparkline from "../components/charts/Sparkline.vue";
import HeatMap from "../components/charts/HeatMap.vue";
import BarMini from "../components/charts/BarMini.vue";
import { jobs, prom } from "../api/client";
import { notify } from "../lib/notify";
import { t } from "../lib/i18n";

// ---------------------------------------------------------------- 状态
const src = ref<"prom" | "none">("prom");
const cluster = ref("");
const ns = ref("");
const job = ref("");
const rangeKey = ref<"1h" | "6h" | "24h" | "7d">("6h");
const groups = ref<Set<string>>(new Set(["throughput", "latency", "resource", "gc"]));

const RANGES = { "1h": 1, "6h": 6, "24h": 24, "7d": 168 } as const;
const RANGE_KEYS = ["1h", "6h", "24h", "7d"] as const;
const yFmtThr = (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}K` : String(Math.round(v)));

function toggle(g: string) {
  const s = new Set(groups.value);
  if (s.has(g)) s.delete(g);
  else s.add(g);
  groups.value = s;
}

// ---------------------------------------------------------------- 基础数据
const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 60_000 });
const items = computed(() => all.data?.value?.items ?? []);
const clusters = computed(() => [...new Set(items.value.map((j) => j.cluster))]);
const namespaces = computed(() => [
  ...new Set(
    items.value
      .filter((j) => !cluster.value || j.cluster === cluster.value)
      .map((j) => j.namespace),
  ),
]);
const jobOptions = computed(() =>
  items.value.filter(
    (j) =>
      (!cluster.value || j.cluster === cluster.value) && (!ns.value || j.namespace === ns.value),
  ),
);

const discovery = useQuery({
  queryKey: ["prom", "discover"],
  queryFn: prom.discover,
  retry: false,
});
const promCount = computed(() => discovery.data?.value?.count ?? 0);
const grafana = computed(() => discovery.data?.value?.items?.find((i) => i.baseUrl)?.baseUrl ?? "");

// 有没有 flink_* 指标（用 count(...) > 0，只回 1 个样本，不再全量扫 metric 名）
const flinkSeries = useQuery({
  queryKey: ["prom", "flink-health"],
  queryFn: () => prom.query('count({__name__=~"flink_.+"}) > 0'),
  retry: false,
  staleTime: 60_000,
  enabled: computed(() => src.value === "prom"),
});
const flinkCount = computed(() => (flinkSeries.data?.value?.data?.result ?? []).length);
const promOn = computed(() => src.value === "prom" && flinkCount.value > 0);

// ---------------------------------------------------------------- 过滤条件
const CLUSTER_JOB_CAP = 40;

// 作业维度：用 job_id（Flink 图里的 job_name 是下划线名，和 Deployment 名对不上）
const jobSel = computed(() => {
  const j = items.value.find((x) => `${x.cluster}/${x.namespace}/${x.name}` === job.value);
  if (!j) return "";
  return j.jobId ? `job_id="${j.jobId}"` : `job_name="${j.name}"`;
});
const nsSel = computed(() => (ns.value ? `namespace="${ns.value}"` : ""));

// 集群维度：Prometheus 侧没有 cluster 标签，只能按该集群的 job_id 集合过滤
const clusterIds = computed(() =>
  !cluster.value
    ? []
    : items.value.filter((j) => j.cluster === cluster.value && j.jobId).map((j) => String(j.jobId)),
);
const clusterTooBig = computed(() => clusterIds.value.length > CLUSTER_JOB_CAP);
const clusterSel = computed(() => {
  const ids = clusterIds.value;
  if (!ids.length || clusterTooBig.value) return "";
  return `job_id=~"${ids.join("|")}"`;
});

const filter = computed(() => {
  const parts = [jobSel.value, nsSel.value, clusterSel.value].filter(Boolean);
  return parts.length ? `{${parts.join(",")}}` : "";
});

// 防抖：一次筛选变化不再连打 9 条 PromQL
const dq = ref({ f: "", h: 6 });
let debounceTimer: ReturnType<typeof setTimeout> | undefined;
watch(
  [filter, rangeKey],
  ([f, k]) => {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      dq.value = { f, h: RANGES[k] };
    }, 300);
  },
  { immediate: true },
);

// 把额外 label 选择器并进已有的 {..}
const withSel = (metric: string, extra: Record<string, string> = {}) => {
  const inner = dq.value.f.replace(/^\{|\}$/g, "");
  const parts = [inner, ...Object.entries(extra).map(([k, v]) => `${k}=~"${v}"`)].filter(Boolean);
  return parts.length ? `${metric}{${parts.join(",")}}` : metric;
};
const M = {
  srcOut: () =>
    withSel("flink_taskmanager_job_task_operator_numRecordsOutPerSecond", {
      task_name: "Source.*",
    }),
  recIn: () => withSel("flink_taskmanager_job_task_operator_numRecordsInPerSecond"),
  recOut: () => withSel("flink_taskmanager_job_task_operator_numRecordsOutPerSecond"),
  busy: () => withSel("flink_taskmanager_job_task_busyTimeMsPerSecond"),
  bp: () => withSel("flink_taskmanager_job_task_backPressuredTimeMsPerSecond"),
  wm: () => withSel("flink_taskmanager_job_task_operator_currentInputWatermark"),
  ckpt: () => withSel("flink_jobmanager_job_lastCheckpointDuration"),
};

// 作业名映射（Prometheus 的 job_name 是下划线版，展示要用 Deployment 名）
const byJobId = computed(() => {
  const m = new Map<string, string>();
  for (const j of items.value) if (j.jobId) m.set(j.jobId, j.name);
  return m;
});
const dispName = (jid?: string, fallback?: string) =>
  (jid && byJobId.value.get(jid)) || fallback || jid || "—";

// ---------------------------------------------------------------- 查询
const now = () => Math.floor(Date.now() / 1000);
const q = (expr: string) => prom.query(expr);

const instant = useQuery({
  queryKey: ["metrics", "instant", dq],
  queryFn: async () => {
    const srcOut = M.srcOut();
    const bp = M.bp();
    const [thr, active, bpOps, ckpt, ckOk, ckBad] = await Promise.all([
      q(`sum(${srcOut})`).catch(() => null),
      q(`count(sum by (job_id) (${srcOut}) > 0) or vector(0)`).catch(() => null),
      // 算子级：先按 (job, task) 聚合再数，标题才配得上"算子"
      q(`count(sum by (job_id, task_name) (${bp}) > 500) or vector(0)`).catch(() => null),
      // JM 指标（PodMonitor 同时选了 jobmanager 后才有）；没采到就保持 NaN，UI 显示 "—"
      q(`max(${M.ckpt()})`).catch(() => null),
      q(`sum(${withSel("flink_jobmanager_job_numberOfCompletedCheckpoints")})`).catch(() => null),
      q(`sum(${withSel("flink_jobmanager_job_numberOfFailedCheckpoints")})`).catch(() => null),
    ]);
    const num = (r: typeof thr) => {
      const raw = r?.data?.result?.[0]?.value?.[1];
      return raw === undefined ? NaN : Number(raw);
    };
    return {
      thr: num(thr),
      active: num(active),
      bpOps: num(bpOps),
      ckpt: num(ckpt),
      ckOk: num(ckOk),
      ckBad: num(ckBad),
    };
  },
  enabled: computed(() => promOn.value),
  refetchInterval: 30_000,
  retry: false,
});

const series = useQuery({
  queryKey: ["metrics", "series", dq],
  queryFn: async () => {
    const end = now();
    const start = end - dq.value.h * 3600;
    const step = Math.max(15, Math.round((dq.value.h * 3600) / 60));
    const [thr, wm, ck] = await Promise.all([
      prom.range(`sum by (job_id, job_name) (${M.srcOut()})`, { start, end, step }),
      prom.range(
        // 空闲 source 的 watermark 是 Long.MIN_VALUE，差值会炸成天文数字；只留 (0, 24h)
        `((time()*1000 - max by (job_id, job_name) (${M.wm()})) / 1000) > 0 < 86400`,
        { start, end, step },
      ),
      prom.range(`max by (job_id, job_name) (${M.ckpt()})`, { start, end, step }).catch(() => null),
    ]);
    return {
      thr: thr.data?.result ?? [],
      wm: wm.data?.result ?? [],
      ckpt: ck?.data?.result ?? [],
      start,
      end,
    };
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

const operators = useQuery({
  queryKey: ["metrics", "operators", dq],
  queryFn: async () => {
    const [i, o, b, p] = await Promise.all([
      q(`topk(30, sum by (job_id, job_name, task_name, operator_name) (${M.recIn()}))`),
      q(`sum by (job_id, job_name, task_name, operator_name) (${M.recOut()})`),
      q(`sum by (job_id, job_name, task_name) (${M.busy()})`),
      q(`sum by (job_id, job_name, task_name) (${M.bp()})`),
    ]);
    const num = (v?: [number, string]) => Number(v?.[1] ?? 0);
    const outMap = new Map<string, number>();
    for (const s of o.data?.result ?? [])
      outMap.set(`${s.metric.job_id}|${s.metric.task_name}`, num(s.value));
    const busyMap = new Map<string, number>();
    for (const s of b.data?.result ?? [])
      busyMap.set(`${s.metric.job_id}|${s.metric.task_name}`, num(s.value));
    const bpMap = new Map<string, number>();
    for (const s of p.data?.result ?? [])
      bpMap.set(`${s.metric.job_id}|${s.metric.task_name}`, num(s.value));

    return (i.data?.result ?? [])
      .map((s) => {
        const k = `${s.metric.job_id}|${s.metric.task_name}`;
        return {
          op: s.metric.operator_name || s.metric.task_name || "—",
          job: dispName(s.metric.job_id, s.metric.job_name),
          inRate: num(s.value),
          outRate: outMap.get(k) ?? 0,
          busy: (busyMap.get(k) ?? 0) / 10,
          bp: bpMap.get(k) ?? 0,
        };
      })
      .sort((a, b2) => b2.inRate - a.inRate)
      .slice(0, 10);
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

const matrix = useQuery({
  queryKey: ["metrics", "matrix", dq],
  queryFn: async () => {
    const end = now();
    const start = end - dq.value.h * 3600;
    const step = Math.max(30, Math.round((dq.value.h * 3600) / 16));
    const r = await prom.range(`sum by (job_id, job_name) (${M.bp()})`, { start, end, step });
    const rows = (r.data?.result ?? []).map((s) => ({
      label: dispName(s.metric.job_id, s.metric.job_name),
      values: (s.values ?? []).map((v) => Math.min(1, Number(v[1]) / 1000)),
    }));
    rows.sort((a, b) => Math.max(...b.values) - Math.max(...a.values));
    return rows.slice(0, 8);
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

const resources = useQuery({
  queryKey: ["metrics", "resources"],
  queryFn: async () => {
    const [cpu, mem, net, meta, gc, net24] = await Promise.all([
      q('100 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))*100'),
      q("100 * (1 - sum(node_memory_MemAvailable_bytes)/sum(node_memory_MemTotal_bytes))"),
      q("sum(rate(node_network_transmit_bytes_total[5m]))/1e6"),
      q(
        "100 * sum(flink_taskmanager_Status_JVM_Memory_Metaspace_Used)/sum(flink_taskmanager_Status_JVM_Memory_Metaspace_Max)",
      ),
      q(
        "sum(rate(flink_taskmanager_Status_JVM_GarbageCollector_MarkSweepCompact_Time[5m])) + sum(rate(flink_taskmanager_Status_JVM_GarbageCollector_Copy_Time[5m]))",
      ).catch(() => null),
      prom.range("sum(rate(node_network_transmit_bytes_total[5m]))/1e6", {
        start: now() - 86400,
        end: now(),
        step: 3600,
      }),
    ]);
    const num = (r: { data?: { result?: { value?: [number, string] }[] } } | null | undefined) =>
      Number(r?.data?.result?.[0]?.value?.[1] ?? NaN);
    return {
      cpu: num(cpu),
      mem: num(mem),
      net: num(net),
      meta: num(meta),
      // rate() 出来的单位已经是 ms/s，不要再 ×1000
      gcMsPerSec: num(gc),
      net24: (net24.data?.result?.[0]?.values ?? []).map((v) => Number(v[1])),
    };
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

const wmTop = useQuery({
  queryKey: ["metrics", "wm-top", dq],
  queryFn: async () => {
    const r = await q(`((time()*1000 - max by (job_id, job_name) (${M.wm()})) / 1000) > 0 < 86400`);
    return (r.data?.result ?? [])
      .map((s) => ({
        job: dispName(s.metric.job_id, s.metric.job_name),
        lag: Number(s.value?.[1] ?? 0),
      }))
      .sort((a, b) => b.lag - a.lag)
      .slice(0, 6);
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

// ---------------------------------------------------------------- 仅 REST 模式：真采样
const sampleOn = ref(false);
const restSample = useQuery({
  queryKey: ["metrics", "rest-sample"],
  queryFn: async () => {
    const running = items.value.filter((j) => j.state === "RUNNING").slice(0, 12);
    const rows: {
      name: string;
      inRate: number;
      outRate: number;
      ckpt?: number;
      degraded?: string;
    }[] = [];
    for (const j of running) {
      try {
        const r = await jobs.liveMetrics(j.cluster, j.namespace, j.name);
        rows.push({
          name: j.name,
          inRate: Number(r.values?.numRecordsInPerSecond ?? 0),
          outRate: Number(r.values?.numRecordsOutPerSecond ?? 0),
          ckpt: Number(r.values?.lastCheckpointDuration ?? NaN),
        });
      } catch (e) {
        rows.push({
          name: j.name,
          inRate: 0,
          outRate: 0,
          degraded: (e as Error).message.slice(0, 60),
        });
      }
    }
    return {
      rows,
      sampled: rows.length,
      total: items.value.filter((j) => j.state === "RUNNING").length,
    };
  },
  enabled: computed(() => sampleOn.value && src.value === "none"),
  refetchInterval: false,
  retry: false,
});

// ---------------------------------------------------------------- 派生
const thrSeries = computed(() => series.data?.value?.thr ?? []);
const topJobs = computed(() => {
  const last = (s: (typeof thrSeries.value)[number]) => Number(s.values?.at(-1)?.[1] ?? 0);
  return [...thrSeries.value].sort((a, b) => last(b) - last(a)).slice(0, 3);
});

const fmtNum = (v: number) =>
  Number.isFinite(v)
    ? v >= 1e6
      ? `${(v / 1e6).toFixed(2)}M`
      : v >= 1e3
        ? `${(v / 1e3).toFixed(1)}K`
        : Math.round(v).toString()
    : "—";
const fmtSec = (v: number) =>
  !Number.isFinite(v)
    ? "—"
    : v >= 3600
      ? `${(v / 3600).toFixed(1)}h`
      : v >= 60
        ? `${(v / 60).toFixed(1)}m`
        : `${v.toFixed(1)}s`;

// 以最长的 series 为准，避免不同 series 点数不一致时 x 轴错位
const timeLabels = computed(() => {
  const longest = [...thrSeries.value].sort(
    (a, b) => (b.values?.length ?? 0) - (a.values?.length ?? 0),
  )[0];
  return (longest?.values ?? []).map((v) => {
    const d = new Date(Number(v[0]) * 1000);
    return dq.value.h <= 24
      ? `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`
      : `${d.getMonth() + 1}/${d.getDate()}`;
  });
});

const thrSum = computed(() => {
  const pts = thrSeries.value[0]?.values ?? [];
  return pts.map((_, i) =>
    thrSeries.value.reduce((a, s) => a + Number(s.values?.[i]?.[1] ?? 0), 0),
  );
});

const thrChart = computed(() => {
  const top = topJobs.value.map((s) => ({
    name: dispName(s.metric.job_id, s.metric.job_name),
    data: (s.values ?? []).map((v) => Number(v[1])),
  }));
  // 没筛选时补一条 Total，保证"图上加起来 = KPI"
  if (!job.value && thrSum.value.length) {
    return [...top, { name: t("met.total"), data: thrSum.value }];
  }
  return top;
});

const thrNow = computed(() => {
  if (!promOn.value) return NaN;
  const fromProm = instant.data?.value?.thr ?? NaN;
  return Number.isFinite(fromProm) ? fromProm : (thrSum.value.at(-1) ?? NaN);
});
const thrPeak = computed(() => (promOn.value ? Math.max(0, ...thrSum.value) : NaN));
const thrDelta = computed(() => {
  if (!promOn.value) return null;
  const a = thrSum.value.at(0) ?? 0;
  const b = thrSum.value.at(-1) ?? 0;
  if (!a) return null;
  return ((b - a) / a) * 100;
});

const activeJobs = computed(() => (promOn.value ? (instant.data?.value?.active ?? NaN) : NaN));
const bpOps = computed(() => (promOn.value ? (instant.data?.value?.bpOps ?? NaN) : NaN));

const wmChart = computed(() => {
  const rows = series.data?.value?.wm ?? [];
  return rows.slice(0, 2).map((s) => ({
    name: dispName(s.metric.job_id, s.metric.job_name),
    data: (s.values ?? []).map((v) => Number(v[1])),
  }));
});

// Checkpoint 时长（JM 指标）：取耗时最长的前 2 个作业
const ckptChart = computed(() => {
  const rows = series.data?.value?.ckpt ?? [];
  const peak = (s: (typeof rows)[number]) =>
    Math.max(0, ...(s.values ?? []).map((v) => Number(v[1])));
  return [...rows]
    .sort((a, b) => peak(b) - peak(a))
    .slice(0, 2)
    .map((s) => ({
      name: dispName(s.metric.job_id, s.metric.job_name),
      data: (s.values ?? []).map((v) => Number(v[1])),
    }));
});
const ckptFailTotal = computed<{ ok: number; bad: number; rate: number } | null>(() => {
  const ok = instant.data?.value?.ckOk ?? NaN;
  const bad = instant.data?.value?.ckBad ?? NaN;
  if (!Number.isFinite(ok) || !Number.isFinite(bad) || ok + bad === 0) return null;
  return { ok, bad, rate: (ok / (ok + bad)) * 100 };
});

// 选中单个作业时可走 REST 取 checkpoint（JM 指标没进 Prometheus 的兜底）
const selJob = computed(() =>
  items.value.find((x) => `${x.cluster}/${x.namespace}/${x.name}` === job.value),
);
const ckptRest = useQuery({
  queryKey: ["metrics", "ckpt-rest", job],
  queryFn: () =>
    jobs.liveMetrics(selJob.value!.cluster, selJob.value!.namespace, selJob.value!.name),
  enabled: computed(() => !!selJob.value),
  retry: false,
});

const ckptValue = computed(() => {
  const fromProm = instant.data?.value?.ckpt ?? NaN;
  if (Number.isFinite(fromProm) && fromProm > 0) return fromProm;
  const fromRest = ckptRest.data?.value?.values?.lastCheckpointDuration;
  return typeof fromRest === "number" ? fromRest : NaN;
});

const busyRows = computed(() => operators.data?.value ?? []);
type BpLevel = readonly [string, string];
const bpLevel = (ms: number): BpLevel =>
  ms > 500
    ? (["HIGH", "danger"] as const)
    : ms > 100
      ? (["MED", "warn"] as const)
      : (["LOW", "ok"] as const);

const matrixRows = computed(() => matrix.data?.value ?? []);
const res = computed(() => resources.data?.value);

const restRows = computed(() => restSample.data?.value?.rows ?? []);
const restTotals = computed(() => ({
  inRate: restRows.value.reduce((n, r) => n + r.inRate, 0),
  outRate: restRows.value.reduce((n, r) => n + r.outRate, 0),
}));

/** 指标分组：必须 computed，否则切语言后 tab 名不重算 */
const metricGroups = computed<Record<string, string>>(() => ({
  throughput: t("met.group.throughput"),
  latency: t("met.group.latency"),
  resource: t("met.group.resource"),
  gc: t("met.group.gc"),
}));

const pageActions = computed(() => [
  {
    label: t("met.refresh"),
    run: () => {
      void flinkSeries.refetch();
      void instant.refetch();
      void series.refetch();
      void operators.refetch();
      void matrix.refetch();
      void resources.refetch();
      void wmTop.refetch();
      if (src.value === "none" && sampleOn.value) void restSample.refetch();
      notify(t("met.refreshed"), "ok");
    },
  },
  {
    label: t("met.grafana"),
    run: () => {
      if (!grafana.value) {
        notify(t("met.noProm"), "warn");
        return;
      }
      window.open(grafana.value, "_blank");
    },
  },
]);
</script>

<template>
  <PageHead
    :title="t('page.metrics')"
    :sub="t('met.sub')"
    :badge="promCount ? t('met.promBadge', { n: promCount }) : t('met.promNone')"
    :tone="promCount ? 'ok' : 'warn'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <!-- 数据源 -->
  <div class="mb-3 rounded-xl border border-line-2 bg-surface px-4 py-2.5">
    <div class="flex flex-wrap items-center gap-3">
      <span class="text-2xs text-fg-2">{{ t("met.source") }}</span>
      <div class="inline-flex overflow-hidden rounded-lg border border-line text-2xs">
        <button
          class="px-3 py-1 transition"
          :class="src === 'prom' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
          @click="src = 'prom'"
        >
          {{ t("met.sourceProm") }}
        </button>
        <button
          class="px-3 py-1 transition"
          :class="src === 'none' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
          @click="src = 'none'"
        >
          {{ t("met.sourceRest") }}
        </button>
      </div>
      <span class="text-2xs text-fg-2">{{ t("met.sourceNote") }}</span>
      <RouterLink to="/settings" class="ml-auto text-2xs text-primary hover:underline">{{
        t("met.changeBackend")
      }}</RouterLink>
    </div>
  </div>

  <!-- 筛选 -->
  <div class="mb-3 rounded-xl border border-line-2 bg-surface px-4 py-2.5">
    <div class="flex flex-wrap items-center gap-2 text-xs">
      <select
        v-model="cluster"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1"
        @change="
          ns = '';
          job = '';
        "
      >
        <option value="">{{ t("met.allClusters") }}</option>
        <option v-for="c in clusters" :key="c" :value="c">{{ c }}</option>
      </select>
      <select
        v-model="ns"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1"
        @change="job = ''"
      >
        <option value="">{{ t("met.allNamespaces") }}</option>
        <option v-for="n in namespaces" :key="n" :value="n">{{ n }}</option>
      </select>
      <select
        v-model="job"
        class="min-w-[180px] rounded-lg border border-line bg-surface-2 px-2 py-1"
      >
        <option value="">{{ t("met.allJobs") }}</option>
        <option
          v-for="j in jobOptions"
          :key="j.name"
          :value="`${j.cluster}/${j.namespace}/${j.name}`"
        >
          {{ j.name }}
        </option>
      </select>
      <div class="inline-flex gap-1">
        <button
          v-for="(g, key) in metricGroups"
          :key="key"
          class="rounded-full border px-2.5 py-0.5 text-2xs transition"
          :class="
            groups.has(key)
              ? 'border-primary/40 bg-primary/10 text-primary'
              : 'border-line text-fg-2 hover:text-fg'
          "
          @click="toggle(key)"
        >
          {{ g }}
        </button>
      </div>
      <div class="ml-auto inline-flex overflow-hidden rounded-lg border border-line text-2xs">
        <button
          v-for="k in RANGE_KEYS"
          :key="k"
          class="px-2.5 py-1 font-mono"
          :class="rangeKey === k ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
          @click="rangeKey = k"
        >
          {{ k }}
        </button>
      </div>
    </div>
    <p v-if="clusterTooBig" class="mt-1.5 text-3xs text-warn">
      {{ t("met.tooManyJobs", { n: clusterIds.length, limit: 40 }) }}
    </p>
  </div>

  <!-- 无 Prometheus：仅 REST 采样 -->
  <div v-if="!promOn" class="mb-3 rounded-xl border border-warn/30 bg-warn/5 px-4 py-3">
    <div class="text-xs font-semibold">{{ t("met.restOnlyTitle") }}</div>
    <p class="mt-1 text-2xs leading-relaxed text-fg-2">
      {{ t("met.restOnlyBody") }}
    </p>
    <div class="mt-2 flex flex-wrap items-center gap-2">
      <button
        class="rounded-lg bg-primary px-2.5 py-1 text-2xs text-white"
        :disabled="restSample.isFetching.value"
        @click="
          sampleOn = true;
          restSample.refetch();
        "
      >
        {{ restSample.isFetching.value ? t("met.sampling") : t("met.sampleLive") }}
      </button>
      <RouterLink
        to="/settings"
        class="rounded-lg border border-line px-2.5 py-1 text-2xs hover:border-primary/50"
        >{{ t("met.configProm") }}</RouterLink
      >
      <RouterLink
        to="/observability"
        class="rounded-lg border border-line px-2.5 py-1 text-2xs hover:border-primary/50"
        >{{ t("met.genPodMonitor") }}</RouterLink
      >
      <span v-if="restSample.data?.value" class="text-2xs text-fg-2">
        {{
          t("met.sampledLine", {
            n: restSample.data.value.sampled,
            total: restSample.data.value.total,
          })
        }}
      </span>
    </div>
    <div
      v-if="restRows.length"
      class="mt-2 overflow-auto rounded-lg border border-line-2 bg-surface"
    >
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-1.5">{{ t("met.col.job") }}</th>
            <th class="px-3 py-1.5 text-right">In</th>
            <th class="px-3 py-1.5 text-right">Out</th>
            <th class="px-3 py-1.5 text-right">Checkpoint</th>
            <th class="px-3 py-1.5">{{ t("met.col.note") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in restRows" :key="r.name" class="border-b border-line-2/60 last:border-0">
            <td class="px-3 py-1 font-mono">{{ r.name }}</td>
            <td class="px-3 py-1 text-right font-mono">{{ fmtNum(r.inRate) }}</td>
            <td class="px-3 py-1 text-right font-mono">{{ fmtNum(r.outRate) }}</td>
            <td class="px-3 py-1 text-right font-mono">
              {{
                Number.isFinite(r.ckpt ?? NaN) ? ((r.ckpt as number) / 1000).toFixed(1) + "s" : "—"
              }}
            </td>
            <td class="px-3 py-1 text-3xs text-fg-2">{{ r.degraded ?? "" }}</td>
          </tr>
          <tr class="border-t border-line-2 bg-surface-2 font-medium">
            <td class="px-3 py-1">{{ t("met.total") }}</td>
            <td class="px-3 py-1 text-right font-mono">{{ fmtNum(restTotals.inRate) }}</td>
            <td class="px-3 py-1 text-right font-mono">{{ fmtNum(restTotals.outRate) }}</td>
            <td colspan="2"></td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

  <!-- KPI -->
  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(210px, 1fr))">
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-primary"></span
        >{{ t("met.entryThroughput") }}
      </div>
      <div class="mt-1 flex items-baseline gap-1">
        <span class="font-mono text-[22px] leading-tight">{{ fmtNum(thrNow) }}</span><small
          class="text-2xs text-fg-2"
          >rec/s</small
        >
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        <span v-if="thrDelta !== null" :class="thrDelta >= 0 ? 'text-ok' : 'text-danger'"
          >{{ thrDelta >= 0 ? "+" : "" }}{{ thrDelta.toFixed(1) }}%</span
        >
        <span class="ml-1">{{ t("met.peak") }} {{ fmtNum(thrPeak) }}</span>
        <span class="ml-1">{{ t("met.sourceSum") }}</span>
      </div>
      <Sparkline class="mt-1" :data="promOn ? thrSum : []" />
    </div>

    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-info"></span>{{ t("met.activeJobs") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ Number.isFinite(activeJobs) ? activeJobs : "—"
        }}<small class="text-2xs text-fg-2"> / {{ items.length }}</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        {{ Number.isFinite(activeJobs) ? t("met.jobsWithIngress") : t("met.promUnavailable") }}
      </div>
      <Sparkline class="mt-1" :data="[]" color="var(--color-info)" />
    </div>

    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-ok"></span>{{ t("met.ckptDuration") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ Number.isFinite(ckptValue) ? (ckptValue / 1000).toFixed(1) : "—"
        }}<small v-if="Number.isFinite(ckptValue)" class="text-2xs text-fg-2">s</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        <template v-if="ckptFailTotal">
          {{
            t("met.rateLine", { rate: ckptFailTotal.rate.toFixed(1), ok: fmtNum(ckptFailTotal.ok) })
          }}
          {{ t("met.failedShort") }}
          {{ fmtNum(ckptFailTotal.bad) }}
        </template>
        <template v-else>{{
          Number.isFinite(ckptValue)
            ? selJob
              ? t("met.fromJm")
              : t("met.jmMetrics")
            : t("met.jmNotCollected")
        }}</template>
      </div>
      <Sparkline class="mt-1" :data="[]" color="var(--color-ok)" />
    </div>

    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-danger"></span>{{ t("met.bpHot") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ Number.isFinite(bpOps) ? bpOps : "—"
        }}<small v-if="Number.isFinite(bpOps)" class="text-2xs text-fg-2">{{
          t("met.count")
        }}</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">{{ t("met.bpNote") }}</div>
      <Sparkline class="mt-1" :data="[]" color="var(--color-danger)" />
    </div>
  </div>

  <!-- 主图：吞吐按作业 -->
  <Card
    :title="job ? t('met.thrCurrent') : t('met.thrTop')"
    :hint="t('met.thrTitle', { range: rangeKey })"
    class="mb-3"
  >
    <template #actions>
      <Badge v-if="!promOn" tone="warn">{{ t("met.needProm") }}</Badge>
      <span v-else class="text-2xs text-fg-2">{{
        job ? t("met.currentJob") : t("met.topN", { n: Math.min(3, thrSeries.length) })
      }}</span>
    </template>
    <LineChart
      :series="promOn ? thrChart : []"
      :labels="timeLabels"
      :height="210"
      :y-fmt="yFmtThr"
      :empty="t('met.promEmpty')"
    />
  </Card>

  <!-- 水位线 + checkpoint -->
  <div v-if="groups.has('latency')" class="mb-3 grid gap-3 md:grid-cols-2">
    <Card :title="t('met.watermarkCard')" :hint="t('met.seconds')">
      <LineChart
        :series="wmChart"
        :labels="timeLabels"
        :height="140"
        :y-fmt="fmtSec"
        :empty="t('met.watermarkEmpty')"
      />
    </Card>
    <Card :title="t('met.ckptCard')" :hint="t('met.ckptHint')">
      <LineChart
        :series="ckptChart"
        :labels="timeLabels"
        :height="140"
        :threshold="{ value: 60000, label: t('met.timeout60') }"
        :empty="t('met.ckptEmpty')"
      />
    </Card>
  </div>

  <!-- 算子热点 + 右侧列 -->
  <div class="mb-3 grid gap-3 xl:grid-cols-[1.6fr_1fr]">
    <Card :title="t('met.hotCard')" :hint="t('met.hotHint')" :pad="false">
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">{{ t("met.col.operator") }}</th>
              <th class="px-3 py-2">{{ t("met.col.job") }}</th>
              <th class="px-3 py-2 text-right">In/Out</th>
              <th class="px-3 py-2 text-right">Busy</th>
              <th class="px-3 py-2">{{ t("met.col.bp") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(r, i) in busyRows" :key="i" class="border-b border-line-2/60 last:border-0">
              <td class="max-w-[260px] truncate px-3 py-1.5 font-mono" :title="r.op">{{ r.op }}</td>
              <td class="whitespace-nowrap px-3 py-1.5 font-mono text-3xs text-fg-2">
                {{ r.job }}
              </td>
              <td class="px-3 py-1.5 text-right font-mono">
                {{ fmtNum(r.inRate) }} / {{ fmtNum(r.outRate) }}
              </td>
              <td class="px-3 py-1.5">
                <div class="flex items-center justify-end gap-2">
                  <div class="h-[6px] w-[56px] overflow-hidden rounded bg-surface-3">
                    <i
                      class="block h-full"
                      :style="{
                        width: Math.min(100, r.busy) + '%',
                        background:
                          r.busy > 85
                            ? 'var(--color-danger)'
                            : r.busy > 65
                              ? 'var(--color-warn)'
                              : 'var(--color-ok)',
                      }"
                    ></i>
                  </div>
                  <span class="w-[34px] text-right font-mono text-3xs"
                    >{{ r.busy.toFixed(0) }}%</span
                  >
                </div>
              </td>
              <td class="px-3 py-1.5">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs font-medium"
                  :class="
                    bpLevel(r.bp)[1] === 'danger'
                      ? 'bg-danger/10 text-danger'
                      : bpLevel(r.bp)[1] === 'warn'
                        ? 'bg-warn/10 text-warn'
                        : 'bg-ok/10 text-ok'
                  "
                  >{{ bpLevel(r.bp)[0] }}</span
                >
              </td>
            </tr>
            <tr v-if="!busyRows.length">
              <td colspan="5" class="px-3 py-6 text-center text-xs text-fg-2">
                {{ t("met.hotEmpty") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>

    <div class="flex flex-col gap-3">
      <Card :title="t('met.bpMatrix')" :hint="t('met.bpMatrixHint')">
        <HeatMap :rows="matrixRows" :cols="16" />
      </Card>

      <Card v-if="groups.has('resource')" :title="t('met.clusterRes')">
        <div class="flex flex-col gap-3">
          <div>
            <div class="mb-1 flex justify-between text-2xs">
              <span class="text-fg-2">CPU</span
              ><span class="font-mono">{{
                Number.isFinite(res?.cpu) ? res!.cpu.toFixed(0) + "%" : "—"
              }}</span>
            </div>
            <div class="h-[7px] overflow-hidden rounded bg-surface-3">
              <i
                class="block h-full"
                :style="{
                  width: Math.min(100, res?.cpu ?? 0) + '%',
                  background: 'var(--color-primary)',
                }"
              ></i>
            </div>
          </div>
          <div>
            <div class="mb-1 flex justify-between text-2xs">
              <span class="text-fg-2">{{ t("met.memory") }}</span><span class="font-mono">{{
                Number.isFinite(res?.mem) ? res!.mem.toFixed(0) + "%" : "—"
              }}</span>
            </div>
            <div class="h-[7px] overflow-hidden rounded bg-surface-3">
              <i
                class="block h-full"
                :style="{
                  width: Math.min(100, res?.mem ?? 0) + '%',
                  background: (res?.mem ?? 0) > 80 ? 'var(--color-warn)' : 'var(--color-ok)',
                }"
              ></i>
            </div>
          </div>
          <div>
            <div class="mb-1 flex justify-between text-2xs">
              <span class="text-fg-2">{{ t("met.netOut") }}</span><span class="font-mono">{{
                Number.isFinite(res?.net) ? res!.net.toFixed(0) + " MB/s" : "—"
              }}</span>
            </div>
            <div class="h-[7px] overflow-hidden rounded bg-surface-3">
              <i
                class="block h-full"
                :style="{
                  width: Math.min(100, ((res?.net ?? 0) / 500) * 100) + '%',
                  background: 'var(--color-info)',
                }"
              ></i>
            </div>
          </div>
          <div>
            <div class="mb-1 flex justify-between text-2xs">
              <span class="text-fg-2">{{ t("met.metaspace") }}</span><span class="font-mono">{{
                Number.isFinite(res?.meta) ? res!.meta.toFixed(0) + "%" : "—"
              }}</span>
            </div>
            <div class="h-[7px] overflow-hidden rounded bg-surface-3">
              <i
                class="block h-full"
                :style="{
                  width: Math.min(100, res?.meta ?? 0) + '%',
                  background: 'var(--color-ok)',
                }"
              ></i>
            </div>
          </div>
          <div>
            <BarMini :data="res?.net24 ?? []" :height="54" />
            <div class="mt-1 text-3xs text-fg-2">{{ t("met.netPeak") }}</div>
          </div>
        </div>
      </Card>

      <Card v-if="groups.has('gc')" :title="t('met.gc')" :hint="t('met.gcHint')">
        <div class="flex flex-col gap-2 text-xs">
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("met.total") }}</span><span class="font-mono">{{
              Number.isFinite(res?.gcMsPerSec) ? res!.gcMsPerSec.toFixed(1) + " ms/s" : "—"
            }}</span>
          </div>
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("met.wallClock") }}</span><span class="font-mono">{{
              Number.isFinite(res?.gcMsPerSec)
                ? ((res!.gcMsPerSec / 1000) * 100).toFixed(1) + "%"
                : "—"
            }}</span>
          </div>
          <p class="text-3xs leading-relaxed text-fg-2">
            {{ t("met.dataSource")
            }}<span class="font-mono">flink_taskmanager_Status_JVM_GarbageCollector_*_Time</span
            >{{ t("met.gcNote") }}
            <span class="font-mono">taskmanager.memory.task.heap.size</span>。
          </p>
        </div>
      </Card>
    </div>
  </div>

  <!-- 水位线滞后 Top -->
  <Card :title="t('met.watermarkTop')" :hint="t('met.watermarkHint')" :pad="false">
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("met.col.job") }}</th>
            <th class="px-3 py-2">Source</th>
            <th class="px-3 py-2 text-right">{{ t("met.col.lag") }}</th>
            <th class="px-3 py-2">{{ t("met.col.state") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="(r, i) in wmTop.data?.value ?? []"
            :key="i"
            class="border-b border-line-2/60 last:border-0"
          >
            <td class="px-3 py-1.5 font-mono">{{ r.job }}</td>
            <td class="px-3 py-1.5 text-3xs text-fg-2">operator currentInputWatermark</td>
            <td class="px-3 py-1.5 text-right font-mono">{{ fmtSec(r.lag) }}</td>
            <td class="px-3 py-1.5">
              <span
                class="rounded-full px-1.5 py-0.5 text-3xs"
                :class="r.lag > 60 ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'"
                >{{ r.lag > 60 ? t("met.watch") : t("met.normal") }}</span
              >
            </td>
          </tr>
          <tr v-if="!(wmTop.data?.value ?? []).length">
            <td colspan="4" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("met.watermarkEmpty2") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>
</template>
