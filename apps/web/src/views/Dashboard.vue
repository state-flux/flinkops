<script setup lang="ts">
import { computed } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink, useRouter } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import LineChart from "../components/charts/LineChart.vue";
import Sparkline from "../components/charts/Sparkline.vue";
import Donut from "../components/charts/Donut.vue";
import { alerts, audit, jobs, prom, snapshots } from "../api/client";
import { notify } from "../lib/notify";
import { isRisk } from "../lib/jobFilters";
import { t } from "../lib/i18n";

const router = useRouter();

const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });
const items = computed(() => all.data?.value?.items ?? []);

const openAlerts = useQuery({
  queryKey: ["alerts", "open"],
  queryFn: () => alerts.events("OPEN"),
  refetchInterval: 60_000,
});
const audits = useQuery({
  queryKey: ["audit"],
  queryFn: () => audit.list(),
  refetchInterval: 60_000,
});
const snaps = useQuery({
  queryKey: ["snapshots", "all"],
  queryFn: () => snapshots.list(),
  retry: false,
});

// ---------------------------------------------------------------- Prometheus
const now = () => Math.floor(Date.now() / 1000);
const hasProm = useQuery({
  queryKey: ["prom", "flink-series"],
  queryFn: () => prom.query('count by (__name__) ({__name__=~"flink_.+"})'),
  retry: false,
});
const promOn = computed(() => (hasProm.data?.value?.data?.result ?? []).length > 0);

const instant = useQuery({
  queryKey: ["dash", "instant"],
  queryFn: async () => {
    const [thr, cpu, mem, pend] = await Promise.all([
      prom
        .query("sum(flink_taskmanager_job_task_operator_numRecordsInPerSecond)")
        .catch(() => null),
      prom.query('100 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))*100').catch(() => null),
      prom
        .query("100 * (1 - sum(node_memory_MemAvailable_bytes)/sum(node_memory_MemTotal_bytes))")
        .catch(() => null),
      prom.query('sum(kube_pod_status_phase{phase="Pending",namespace="flink"})').catch(() => null),
    ]);
    const num = (r: typeof thr) => Number(r?.data?.result?.[0]?.value?.[1] ?? NaN);
    return { thr: num(thr), cpu: num(cpu), mem: num(mem), pending: num(pend) };
  },
  enabled: computed(() => promOn.value),
  refetchInterval: 30_000,
  retry: false,
});

const series = useQuery({
  queryKey: ["dash", "series"],
  queryFn: async () => {
    const end = now();
    const start = end - 24 * 3600;
    const step = 900;
    const [thr, active, bp] = await Promise.all([
      prom.range("sum(flink_taskmanager_job_task_operator_numRecordsInPerSecond)", {
        start,
        end,
        step,
      }),
      prom.range(
        "count(sum by (job_id) (flink_taskmanager_job_task_operator_numRecordsInPerSecond) > 0)",
        { start, end, step },
      ),
      prom.range(
        "count(sum by (job_id) (flink_taskmanager_job_task_backPressuredTimeMsPerSecond) > 500)",
        { start, end, step },
      ),
    ]);
    return {
      thr: thr.data?.result?.[0]?.values ?? [],
      active: active.data?.result?.[0]?.values ?? [],
      bp: bp.data?.result?.[0]?.values ?? [],
    };
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

// 每作业背压（用于"需关注作业"表的背压列）
const bpByJob = useQuery({
  queryKey: ["dash", "bpByJob"],
  queryFn: async () => {
    const r = await prom.query(
      "sum by (job_id) (flink_taskmanager_job_task_backPressuredTimeMsPerSecond)",
    );
    const m = new Map<string, number>();
    for (const s of r.data?.result ?? []) m.set(s.metric.job_id ?? "", Number(s.value?.[1] ?? 0));
    return m;
  },
  enabled: computed(() => promOn.value),
  retry: false,
});

// ---------------------------------------------------------------- 派生
const total = computed(() => items.value.length);
const running = computed(() => items.value.filter((j) => j.state === "RUNNING").length);
const failed = computed(
  () => items.value.filter((j) => ["FAILED", "ERROR"].includes(j.state)).length,
);
const attention = computed(
  () => items.value.filter((j) => j.state === "FAILED" || j.health === "red").length,
);
const restartSum = computed(() => items.value.reduce((n, j) => n + (j.restartNonce ?? 0), 0));
const restartedJobs = computed(() => items.value.filter((j) => (j.restartNonce ?? 0) > 0).length);
const slotsTotal = computed(() =>
  items.value.reduce((n, j) => n + (j.taskManagerReplicas ?? 0) * (j.slotsPerTm ?? 0), 0),
);
const tmCount = computed(() => items.value.reduce((n, j) => n + (j.taskManagerReplicas ?? 0), 0));
const jmCount = computed(() => items.value.filter((j) => j.jobManagerStatus === "READY").length);
const healthPct = computed(() =>
  total.value ? Math.round((running.value / total.value) * 100) : 0,
);

const stateItems = computed(() => {
  const m = new Map<string, number>();
  for (const j of items.value) m.set(j.state || "UNKNOWN", (m.get(j.state || "UNKNOWN") ?? 0) + 1);
  const colors: Record<string, string> = {
    RUNNING: "#059669",
    RECONCILING: "#d97706",
    RESTARTING: "#d97706",
    FAILED: "#b91c1c",
    SUSPENDED: "#64748b",
    FINISHED: "#0891b2",
    CREATED: "#7c3aed",
  };
  return [...m.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([label, value]) => ({ label, value, color: colors[label] ?? "#94a3b8" }));
});

const bpOf = (jobId?: string) => (jobId ? (bpByJob.data?.value?.get(jobId) ?? 0) : 0);
const bpLevel = (ms: number) => (ms > 500 ? "HIGH" : ms > 100 ? "MED" : "LOW");

const watchRows = computed(() =>
  [...items.value]
    // 用共享口径（FAILED / red）而不是 health !== green：
    // SUSPENDED/CREATED 是过程态，不该挤占"需关注"名额
    .filter(isRisk)
    .slice(0, 6)
    .map((j) => ({
      name: j.name,
      cluster: j.cluster,
      namespace: j.namespace,
      state: j.state,
      health: j.health,
      signal: ["FAILED", "ERROR"].includes(j.state)
        ? "FAILED"
        : ["RECONCILING", "RESTARTING", "CREATED"].includes(j.state)
          ? "RECONCILING"
          : ["SUSPENDED", "FINISHED"].includes(j.state)
            ? "MANUAL_STOP"
            : "UNKNOWN",
      bp: bpOf(j.jobId),
      updatedAt: j.updatedAt,
    })),
);

const trend = computed(() => {
  const s = series.data?.value;
  const labels = (s?.active ?? []).map((v) => {
    const d = new Date(Number(v[0]) * 1000);
    return `${String(d.getHours()).padStart(2, "0")}:00`;
  });
  return {
    labels,
    chart: [
      { name: t("dash.jobsWithThroughput2"), data: (s?.active ?? []).map((v) => Number(v[1])) },
      { name: t("dash.bpHIGHJobs"), data: (s?.bp ?? []).map((v) => Number(v[1])) },
    ],
  };
});

const thrData = computed<number[]>(() => (series.data?.value?.thr ?? []).map((v) => Number(v[1])));
const thrPeak = computed(() => Math.round(Math.max(0, ...thrData.value)));

const alertRows = computed(() => (openAlerts.data?.value?.items ?? []).slice(0, 5));
const sevTone = (s: string) => (s === "danger" ? "danger" : s === "warn" ? "warn" : "info");

const auditRows = computed(() => (audits.data?.value?.items ?? []).slice(0, 7));
const ACTION_LABEL: Record<string, string> = {
  "job.start": t("dash.act.jobStart"),
  "job.stop": t("dash.act.jobStop"),
  "job.restart": t("dash.act.jobRestart"),
  "job.delete": t("dash.act.jobDelete"),
  "savepoint.trigger": t("dash.act.savepoint"),
  "job.savepoint": t("dash.act.savepoint"),
  "auth.login": t("dash.act.login"),
  "ui.open": t("dash.act.openUi"),
  "proxy.blocked": t("dash.act.proxyBlocked"),
  "alert.ack": t("dash.act.alertAck"),
  "jobs.sync": t("dash.act.jobsSync"),
};
const rel = (ts?: string) => {
  if (!ts) return "—";
  const d = (Date.now() - new Date(ts).getTime()) / 1000;
  if (!Number.isFinite(d)) return "—";
  if (d < 60) return `${Math.max(1, Math.round(d))}s`;
  if (d < 3600) return `${Math.round(d / 60)}m`;
  if (d < 86400) return `${Math.round(d / 3600)}h`;
  return `${Math.round(d / 86400)}d`;
};

const snapshotStats = computed(() => {
  const list = snaps.data?.value?.items ?? [];
  return {
    ok: list.filter((s) => s.state === "COMPLETED").length,
    bad: list.filter((s) => ["FAILED", "ABORTED"].includes(String(s.state))).length,
    total: list.length,
  };
});

const clusterIds = computed(() => [...new Set(items.value.map((j) => j.cluster))]);
// 每个集群单独判风险（原来复用了全局 attention，一个集群有问题所有集群都标红）
const clusterRisk = (c: string) =>
  items.value.some((j) => j.cluster === c && (j.state === "FAILED" || j.health === "red"));

const pageActions = computed(() => [
  {
    label: t("dash.refresh"),
    run: () => {
      void all.refetch();
      void openAlerts.refetch();
      void audits.refetch();
      void snaps.refetch();
      notify(t("dash.refreshed"), "ok");
    },
  },
  { label: t("dash.submitJob"), run: () => router.push("/submit") },
]);
</script>

<template>
  <PageHead
    :title="t('page.overview')"
    :sub="t('dash.sub', { n: total })"
    :badge="attention ? t('dash.badgeAttention', { n: attention }) : t('dash.badgeHealthy')"
    :tone="attention ? 'warn' : 'ok'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <!-- KPI -->
  <div class="mb-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-5">
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-ok"></span>{{ t("dash.running") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ running }}<small class="text-2xs text-fg-2"> / {{ total }}</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        {{ t("dash.failedAttention", { failed, attention }) }}
      </div>
    </div>
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-danger"></span
        >{{ t("dash.restarts") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ restartSum }}<small class="text-2xs text-fg-2">{{ t("dash.times") }}</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        {{ t("dash.restartedLine", { n: restartedJobs }) }}
      </div>
    </div>
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-primary"></span>{{ t("dash.rate") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ Number.isFinite(instant.data?.value?.thr) ? Math.round(instant.data!.value!.thr) : "—"
        }}<small class="text-2xs text-fg-2"> rec/s</small>
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">{{ t("dash.peakLine", { peak: thrPeak }) }}</div>
      <Sparkline class="mt-1" :data="thrData" />
    </div>
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-violet"></span
        >{{ t("dash.clusterCpuMem") }}
      </div>
      <div class="mt-1 font-mono text-[22px] leading-tight">
        {{ Number.isFinite(instant.data?.value?.cpu) ? Math.round(instant.data!.value!.cpu) : "—"
        }}<small class="text-2xs text-fg-2"
          >% /
          {{
            Number.isFinite(instant.data?.value?.mem) ? Math.round(instant.data!.value!.mem) : "—"
          }}%</small
        >
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        {{ t("dash.slotsLine", { slots: slotsTotal, tm: tmCount }) }}
      </div>
    </div>
    <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
      <div class="flex items-center gap-1.5 text-2xs text-fg-2">
        <span class="inline-block h-1.5 w-1.5 rounded-full bg-warn"></span>{{ t("dash.ckptRate") }}
      </div>
      <div class="mt-1 font-mono text-[15px] leading-tight text-fg-2">
        {{ t("dash.notCollected") }}
      </div>
      <div class="mt-0.5 text-2xs text-fg-2">
        {{ t("dash.jmLine", { ok: snapshotStats.ok, total: snapshotStats.total }) }}
      </div>
    </div>
  </div>

  <!-- 趋势 + 资源水位 -->
  <div class="mb-3 grid gap-3 xl:grid-cols-[1.6fr_1fr]">
    <Card :title="t('dash.trend')" :hint="t('dash.trendHint')">
      <LineChart
        :series="trend.chart"
        :labels="trend.labels"
        :height="190"
        :y-fmt="(v: number) => String(Math.round(v))"
        :empty="t('dash.promDown')"
      />
    </Card>

    <Card :title="t('dash.resources')">
      <div class="flex flex-col gap-3">
        <div>
          <div class="mb-1 flex justify-between text-2xs">
            <span class="text-fg-2">CPU</span
            ><span class="font-mono">{{
              Number.isFinite(instant.data?.value?.cpu)
                ? Math.round(instant.data!.value!.cpu) + "%"
                : "—"
            }}</span>
          </div>
          <div class="h-[7px] overflow-hidden rounded bg-surface-3">
            <i
              class="block h-full"
              :style="{
                width: Math.min(100, instant.data?.value?.cpu ?? 0) + '%',
                background: 'var(--color-primary)',
              }"
            ></i>
          </div>
        </div>
        <div>
          <div class="mb-1 flex justify-between text-2xs">
            <span class="text-fg-2">{{ t("dash.memory") }}</span><span class="font-mono">{{
              Number.isFinite(instant.data?.value?.mem)
                ? Math.round(instant.data!.value!.mem) + "%"
                : "—"
            }}</span>
          </div>
          <div class="h-[7px] overflow-hidden rounded bg-surface-3">
            <i
              class="block h-full"
              :style="{
                width: Math.min(100, instant.data?.value?.mem ?? 0) + '%',
                background: 'var(--color-warn)',
              }"
            ></i>
          </div>
        </div>
        <div>
          <div class="mb-1 flex justify-between text-2xs">
            <span class="text-fg-2">{{ t("dash.tmSlots") }}</span><span class="font-mono">{{
              t("dash.slotsCount", { n: slotsTotal })
            }}</span>
          </div>
          <div class="h-[7px] overflow-hidden rounded bg-surface-3">
            <i class="block h-full w-full" style="background: var(--color-info)"></i>
          </div>
        </div>
        <div class="flex items-center gap-3 border-t border-line-2 pt-3">
          <Donut
            :items="[
              { label: t('dash.runningOnly'), value: running, color: '#059669' },
              { label: t('dash.others'), value: Math.max(0, total - running), color: '#94a3b8' },
            ]"
            :size="82"
            :center-label="t('dash.availability')"
            :center-value="`${healthPct}%`"
          />
          <div class="flex-1 text-2xs">
            <div class="flex justify-between">
              <span class="text-fg-2">{{ t("dash.tmInstances") }}</span><span class="font-mono">{{
                tmCount
              }}</span>
            </div>
            <div class="mt-1 flex justify-between">
              <span class="text-fg-2">JobManager</span><span class="font-mono">{{ jmCount }}</span>
            </div>
            <div class="mt-1 flex justify-between">
              <span class="text-fg-2">{{ t("dash.pendingPods") }}</span>
              <span
                class="font-mono"
                :class="(instant.data?.value?.pending ?? 0) > 0 ? 'text-warn' : ''"
                >{{
                  Number.isFinite(instant.data?.value?.pending) ? instant.data!.value!.pending : "—"
                }}</span
              >
            </div>
          </div>
        </div>
      </div>
    </Card>
  </div>

  <!-- 集群 / 分布 / 告警 -->
  <div class="mb-3 grid gap-3 md:grid-cols-3">
    <Card :title="t('dash.health')">
      <template #actions
        ><RouterLink to="/clusters" class="text-2xs text-primary hover:underline">{{
          t("dash.allArrow")
        }}</RouterLink></template
      >
      <div class="flex flex-col gap-3">
        <div v-for="c in clusterIds" :key="c">
          <div class="mb-1 flex items-center justify-between">
            <span class="text-xs font-medium">{{ c }}</span>
            <span
              class="rounded-full px-1.5 py-0.5 text-3xs"
              :class="clusterRisk(c) ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'"
              >{{ clusterRisk(c) ? t("dash.risk") : t("dash.healthy") }}</span
            >
          </div>
          <div class="h-[6px] overflow-hidden rounded bg-surface-3">
            <i
              class="block h-full"
              :style="{
                width: Math.min(100, instant.data?.value?.cpu ?? 0) + '%',
                background:
                  (instant.data?.value?.cpu ?? 0) > 80 ? 'var(--color-warn)' : 'var(--color-ok)',
              }"
            ></i>
          </div>
          <!-- CPU/内存是节点级全局指标，没有 cluster 维度：这里必须标注"全局"，否则每个集群显示同一个数 -->
          <div class="mt-1 text-3xs text-fg-2">
            {{
              t("dash.cpuLine", {
                n: items.filter((j) => j.cluster === c).length,
                cpu: Math.round(instant.data?.value?.cpu ?? 0),
              })
            }}
          </div>
        </div>
      </div>
    </Card>

    <Card :title="t('dash.stateDist')" :hint="`${total} 个资源`">
      <Donut
        :items="stateItems"
        :size="104"
        :center-label="t('dash.availability')"
        :center-value="`${healthPct}%`"
      />
      <div class="mt-3 flex justify-between text-2xs text-fg-2">
        <span>{{ t("dash.savepointTriggers") }}</span><span class="font-mono">{{
          t("dash.savepointLine", { ok: snapshotStats.ok, bad: snapshotStats.bad })
        }}</span>
      </div>
      <div class="mt-1 flex justify-between text-2xs text-fg-2">
        <span>{{ t("dash.restartTotal") }}</span><span class="font-mono">{{
          t("dash.restartLine", { n: restartSum })
        }}</span>
      </div>
    </Card>

    <Card :title="t('dash.activeAlerts')" :pad="false">
      <template #actions
        ><RouterLink to="/alerts" class="text-2xs text-primary hover:underline">{{
          t("dash.allArrowN", { n: openAlerts.data?.value?.open ?? 0 })
        }}</RouterLink></template
      >
      <div class="divide-y divide-line-2/70">
        <div v-for="a in alertRows" :key="a.id" class="flex items-center gap-2 px-4 py-2">
          <Badge :tone="sevTone(a.severity)">{{
            a.severity === "danger" ? "P1" : a.severity === "warn" ? "P2" : "P3"
          }}</Badge>
          <div class="min-w-0 flex-1">
            <div class="truncate text-xs">{{ a.message }}</div>
            <div class="text-3xs text-fg-2">{{ a.job }} · {{ a.namespace }}</div>
          </div>
          <span class="shrink-0 text-3xs text-fg-2">{{ rel(a.ts) }}</span>
        </div>
        <div v-if="!alertRows.length" class="px-4 py-6 text-center text-xs text-fg-2">
          {{ t("dash.noOpenAlerts") }}
        </div>
      </div>
    </Card>
  </div>

  <!-- 需关注作业 + 最近活动 -->
  <div class="grid gap-3 xl:grid-cols-[1.8fr_1fr]">
    <Card :title="t('dash.attentionJobs')" :hint="t('dash.attentionHint')" :pad="false">
      <template #actions
        ><RouterLink to="/jobs" class="text-2xs text-primary hover:underline">{{
          t("dash.gotoJobs")
        }}</RouterLink></template
      >
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">{{ t("dash.col.job") }}</th>
              <th class="px-3 py-2">{{ t("dash.col.state") }}</th>
              <th class="px-3 py-2">{{ t("dash.col.risk") }}</th>
              <th class="px-3 py-2">{{ t("dash.col.bp") }}</th>
              <th class="px-3 py-2">{{ t("dash.col.changed") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="r in watchRows"
              :key="r.name"
              class="cursor-pointer border-b border-line-2/60 last:border-0 hover:bg-hover"
              @click="router.push(`/jobs/${r.cluster}/${r.namespace}/${r.name}`)"
            >
              <td class="px-3 py-2">
                <div class="font-medium">{{ r.name }}</div>
                <div class="font-mono text-3xs text-fg-2">{{ r.cluster }} / {{ r.namespace }}</div>
              </td>
              <td class="px-3 py-2">
                <Badge
                  :tone="r.state === 'FAILED' ? 'danger' : r.state === 'RUNNING' ? 'ok' : 'warn'"
                  >{{ r.state }}</Badge
                >
              </td>
              <td class="px-3 py-2">
                <span
                  class="rounded border px-1 py-0.5 font-mono text-3xs"
                  :class="
                    r.state === 'FAILED'
                      ? 'border-danger/30 text-danger'
                      : 'border-warn/30 text-warn'
                  "
                  >{{ r.signal }}</span
                >
              </td>
              <td class="px-3 py-2">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs"
                  :class="
                    bpLevel(r.bp) === 'HIGH'
                      ? 'bg-danger/10 text-danger'
                      : bpLevel(r.bp) === 'MED'
                        ? 'bg-warn/10 text-warn'
                        : 'bg-ok/10 text-ok'
                  "
                  >{{ bpLevel(r.bp) }}</span
                >
              </td>
              <td class="whitespace-nowrap px-3 py-2 text-3xs text-fg-2">
                {{ r.updatedAt ? t("dash.ago", { rel: rel(r.updatedAt) }) : "—" }}
              </td>
            </tr>
            <tr v-if="!watchRows.length">
              <td colspan="5" class="px-3 py-6 text-center text-xs text-fg-2">
                {{ t("dash.allHealthy") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>

    <Card :title="t('dash.recent')" :hint="t('dash.audit')">
      <div class="flex flex-col gap-2">
        <div v-for="a in auditRows" :key="a.id" class="flex gap-2">
          <span
            class="mt-1 inline-block h-1.5 w-1.5 shrink-0 rounded-full"
            :class="a.result === 'ok' ? 'bg-ok' : a.result === 'denied' ? 'bg-warn' : 'bg-danger'"
          ></span>
          <div class="min-w-0 flex-1">
            <div class="flex items-baseline gap-1.5">
              <span class="text-xs">{{ ACTION_LABEL[a.action] ?? a.action }}</span>
              <span class="ml-auto shrink-0 text-3xs text-fg-2">{{ rel(a.ts) }}</span>
            </div>
            <div class="truncate text-3xs text-fg-2">{{ a.target || "—" }} · {{ a.actor }}</div>
          </div>
        </div>
        <div v-if="!auditRows.length" class="py-4 text-center text-xs text-fg-2">
          {{ t("dash.noAudit") }}
        </div>
      </div>
    </Card>
  </div>
</template>
