<script setup lang="ts">
import { computed } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Donut from "../components/charts/Donut.vue";
import LineChart from "../components/charts/LineChart.vue";
import { jobs, prom } from "../api/client";
import { download, notify } from "../lib/notify";
import { t } from "../lib/i18n";

const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });
const items = computed(() => all.data?.value?.items ?? []);

const promOn = useQuery({
  queryKey: ["prom", "flink-series"],
  queryFn: () => prom.query('count by (__name__) ({__name__=~"flink_.+"})'),
  retry: false,
});
const hasProm = computed(() => (promOn.data?.value?.data?.result ?? []).length > 0);

// Pod requests / 数量 / 节点可分配
const usage = useQuery({
  queryKey: ["ns", "usage"],
  queryFn: async () => {
    const [cpu, mem, pods, capCpu, capMem] = await Promise.all([
      prom.query('sum by (namespace) (kube_pod_container_resource_requests{resource="cpu"})'),
      prom.query('sum by (namespace) (kube_pod_container_resource_requests{resource="memory"})'),
      prom.query("count by (namespace) (kube_pod_info)"),
      prom.query('sum(kube_node_status_allocatable{resource="cpu"})'),
      prom.query('sum(kube_node_status_allocatable{resource="memory"})'),
    ]);
    const num = (v?: [number, string]) => Number(v?.[1] ?? 0);
    return {
      cpu: new Map((cpu.data?.result ?? []).map((s) => [s.metric.namespace ?? "", num(s.value)])),
      mem: new Map((mem.data?.result ?? []).map((s) => [s.metric.namespace ?? "", num(s.value)])),
      pods: new Map((pods.data?.result ?? []).map((s) => [s.metric.namespace ?? "", num(s.value)])),
      capCpu: num(capCpu.data?.result?.[0]?.value),
      capMem: num(capMem.data?.result?.[0]?.value),
    };
  },
  enabled: computed(() => hasProm.value),
  retry: false,
});

const trend = useQuery({
  queryKey: ["ns", "trend"],
  queryFn: async () => {
    const end = Math.floor(Date.now() / 1000);
    const r = await prom.range(
      'sum by (namespace) (kube_pod_container_resource_requests{resource="cpu",namespace="flink"})',
      { start: end - 24 * 3600, end, step: 900 },
    );
    const rows = r.data?.result ?? [];
    return {
      labels: (rows[0]?.values ?? []).map((v) => {
        const d = new Date(Number(v[0]) * 1000);
        return `${String(d.getHours()).padStart(2, "0")}:00`;
      }),
      series: rows.map((s) => ({
        name: s.metric.namespace ?? "—",
        data: (s.values ?? []).map((v) => Number(v[1])),
      })),
    };
  },
  enabled: computed(() => hasProm.value),
  retry: false,
});

const rows = computed(() => {
  const m = new Map<
    string,
    { ns: string; jobs: number; running: number; slots: number; nodeport: number; failed: number }
  >();
  for (const j of items.value) {
    const key = j.namespace;
    const e = m.get(key) ?? { ns: key, jobs: 0, running: 0, slots: 0, nodeport: 0, failed: 0 };
    e.jobs += 1;
    if (j.state === "RUNNING") e.running += 1;
    if (j.health === "red") e.failed += 1;
    e.slots += (j.taskManagerReplicas ?? 0) * (j.slotsPerTm ?? 0);
    if (j.restExposed === "NodePort") e.nodeport += 1;
    m.set(key, e);
  }
  const u = usage.data?.value;
  return [...m.values()]
    .map((e) => ({
      ...e,
      cpu: u?.cpu.get(e.ns) ?? NaN,
      memGiB: u ? (u.mem.get(e.ns) ?? 0) / 1024 ** 3 : NaN,
      pods: u?.pods.get(e.ns) ?? NaN,
    }))
    .sort((a, b) => b.jobs - a.jobs);
});

const totalCpu = computed(() =>
  rows.value.reduce((n, r) => n + (Number.isFinite(r.cpu) ? r.cpu : 0), 0),
);
const totalMem = computed(() =>
  rows.value.reduce((n, r) => n + (Number.isFinite(r.memGiB) ? r.memGiB : 0), 0),
);
const capCpu = computed(() => usage.data?.value?.capCpu ?? NaN);
const capMem = computed(() => usage.data?.value?.capMem ?? NaN);

const donutItems = computed(() =>
  rows.value.map((r) => ({
    label: r.ns,
    value: Number.isFinite(r.cpu) ? Math.round(r.cpu * 10) / 10 : 0,
  })),
);

const pageActions = computed(() => [
  {
    label: t("ns.export"),
    run: () => {
      const head = "cluster,namespace,jobs,running,failed,slots,nodeport,cpu_requests,mem_gib,pods";
      const body = rows.value.map((r) =>
        [
          "local",
          r.ns,
          r.jobs,
          r.running,
          r.failed,
          r.slots,
          r.nodeport,
          Number.isFinite(r.cpu) ? r.cpu.toFixed(2) : "",
          Number.isFinite(r.memGiB) ? r.memGiB.toFixed(1) : "",
          Number.isFinite(r.pods) ? r.pods : "",
        ].join(","),
      );
      download(
        `flinkops-namespaces-${Date.now()}.csv`,
        [head, ...body].join("\n"),
        "text/csv;charset=utf-8",
      );
      notify(t("ns.exported", { n: rows.value.length }), "ok");
    },
  },
]);
</script>

<template>
  <PageHead :title="t('page.namespaces')" :sub="t('ns.sub', { n: rows.length })">
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <!-- 配额总览 -->
  <div class="mb-3 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
    <Kpi
      :label="t('ns.col.ns')"
      :value="rows.length"
      :foot="t('ns.jobsCount', { n: items.length })"
    />
    <Kpi
      :label="t('ns.kpi.cpu')"
      :value="
        t('ns.cpuValue', {
          used: Number.isFinite(totalCpu) ? totalCpu.toFixed(1) : '—',
          cap: Number.isFinite(capCpu) ? Math.round(capCpu) : '—',
        })
      "
      :foot="
        Number.isFinite(capCpu) && capCpu
          ? t('ns.usage', { pct: ((totalCpu / capCpu) * 100).toFixed(1) })
          : ''
      "
    />
    <Kpi
      :label="t('ns.kpi.mem')"
      :value="
        t('ns.memValue', {
          used: Number.isFinite(totalMem) ? totalMem.toFixed(0) : '—',
          cap: Number.isFinite(capMem) ? Math.round(capMem / 1024 ** 3) : '—',
        })
      "
      :foot="
        Number.isFinite(capMem) && capMem
          ? t('ns.usage', { pct: ((totalMem / (capMem / 1024 ** 3)) * 100).toFixed(1) })
          : ''
      "
    />
    <Kpi :label="t('ns.quota')" :value="t('ns.notWired')" :foot="t('ns.quotaFoot')" tone="warn" />
  </div>

  <div class="mb-3 grid gap-3 xl:grid-cols-[1fr_1.4fr]">
    <Card :title="t('ns.capacityCard')" :hint="t('ns.capacityHint')">
      <Donut
        :items="donutItems"
        :size="118"
        :center-label="t('ns.cores')"
        :center-value="`${totalCpu.toFixed(1)}`"
      />
      <p class="mt-2 text-3xs leading-relaxed text-fg-2">
        {{ t("ns.sourcePrefix") }}<span class="font-mono">kube_pod_container_resource_requests</span
        >{{ t("ns.sourceNote") }}
      </p>
    </Card>
    <Card :title="t('ns.trendCard')" :hint="t('ns.trendHint')">
      <LineChart
        :series="trend.data?.value?.series ?? []"
        :labels="trend.data?.value?.labels ?? []"
        :height="170"
        :y-fmt="(v: number) => v.toFixed(1)"
        :empty="t('ns.promEmpty')"
      />
    </Card>
  </div>

  <!-- 明细 -->
  <Card :title="t('ns.detailCard')" :pad="false">
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("ns.col.cluster") }}</th>
            <th class="px-3 py-2">{{ t("ns.kpi.ns") }}</th>
            <th class="px-3 py-2 text-right">{{ t("ns.col.jobs") }}</th>
            <th class="px-3 py-2 text-right">{{ t("ns.col.running") }}</th>
            <th class="px-3 py-2 text-right">{{ t("ns.col.failed") }}</th>
            <th class="px-3 py-2 text-right">{{ t("ns.col.slots") }}</th>
            <th class="px-3 py-2 text-right">CPU req</th>
            <th class="px-3 py-2 text-right">{{ t("ns.col.memReq") }}</th>
            <th class="px-3 py-2 text-right">Pod</th>
            <th class="px-3 py-2 text-right">NodePort</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in rows" :key="r.ns" class="border-b border-line-2/60 last:border-0">
            <td class="px-3 py-1.5 font-mono text-2xs">local</td>
            <td class="px-3 py-1.5 font-mono">{{ r.ns }}</td>
            <td class="px-3 py-1.5 text-right font-mono">{{ r.jobs }}</td>
            <td class="px-3 py-1.5 text-right font-mono text-ok">{{ r.running }}</td>
            <td
              class="px-3 py-1.5 text-right font-mono"
              :class="r.failed ? 'text-danger' : 'text-fg-2'"
            >
              {{ r.failed }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">{{ r.slots }}</td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(r.cpu) ? r.cpu.toFixed(1) : "—" }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(r.memGiB) ? r.memGiB.toFixed(1) + " GiB" : "—" }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(r.pods) ? r.pods : "—" }}
            </td>
            <td
              class="px-3 py-1.5 text-right font-mono"
              :class="r.nodeport ? 'text-warn' : 'text-fg-2'"
            >
              {{ r.nodeport }}
            </td>
          </tr>
          <tr v-if="!rows.length">
            <td colspan="10" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("ns.noJobs") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p class="border-t border-line-2 px-3 py-2 text-2xs text-fg-2">
      {{ t("ns.note") }}
    </p>
  </Card>
</template>
