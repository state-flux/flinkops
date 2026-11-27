<script setup lang="ts">
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink, useRouter } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Kpi from "../components/Kpi.vue";
import Card from "../components/Card.vue";
import DataTable from "../components/DataTable.vue";
import Badge from "../components/Badge.vue";
import Modal from "../components/Modal.vue";
import EmptyState from "../components/EmptyState.vue";
import { admin, jobs, prom } from "../api/client";
import { notify } from "../lib/notify";
import LineChart from "../components/charts/LineChart.vue";
import { t } from "../lib/i18n";

const p = defineProps<{ id: string }>();
const tab = ref("overview");
const router = useRouter();
const credOpen = ref(false);
const cred = ref<Record<string, unknown> | null>(null);

const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });

const mine = computed(() => (all.data?.value?.items ?? []).filter((j) => j.cluster === p.id));

const byNs = computed(() => {
  const m = new Map<
    string,
    { ns: string; jobs: number; green: number; nodeport: number; slots: number }
  >();
  for (const j of mine.value) {
    const e = m.get(j.namespace) ?? { ns: j.namespace, jobs: 0, green: 0, nodeport: 0, slots: 0 };
    e.jobs += 1;
    if (j.health === "green") e.green += 1;
    if (j.restExposed === "NodePort") e.nodeport += 1;
    e.slots += (j.taskManagerReplicas ?? 0) * (j.slotsPerTm ?? 0);
    m.set(j.namespace, e);
  }
  return [...m.values()];
});

// ---- 节点与资源曲线（Prometheus / kube-state-metrics）

const nodeRows = useQuery({
  queryKey: ["cluster", p.id, "nodes"],
  queryFn: async () => {
    const [info, cpu, mem, pods, flinkPods, ready, roles] = await Promise.all([
      prom.query("kube_node_info"),
      prom.query('sum by (node) (kube_node_status_allocatable{resource="cpu"})'),
      prom.query('sum by (node) (kube_node_status_allocatable{resource="memory"})'),
      prom.query("count by (node) (kube_pod_info)"),
      prom.query('count by (node) (kube_pod_info{namespace="flink"})'),
      prom.query('kube_node_status_condition{condition="Ready",status="true"}'),
      prom.query('kube_node_role{role=~"control-plane|master"}'),
    ]);
    const roleSet = new Set((roles.data?.result ?? []).map((s) => s.metric.node ?? ""));
    const toMap = (r: typeof cpu) => {
      const m = new Map<string, number>();
      for (const s of r?.data?.result ?? []) m.set(s.metric.node ?? "", Number(s.value?.[1] ?? 0));
      return m;
    };
    const cm = toMap(cpu);
    const mm = toMap(mem);
    const pm = toMap(pods);
    const fm = toMap(flinkPods);
    const rm = toMap(ready);
    return (info.data?.result ?? [])
      .map((s) => {
        const node = s.metric.node ?? "—";
        return {
          node,
          role: roleSet.has(node) ? "control-plane" : "worker",
          cpu: cm.get(node) ?? NaN,
          memGiB: (mm.get(node) ?? 0) / 1024 ** 3,
          pods: pm.get(node) ?? NaN,
          flinkPods: fm.get(node) ?? 0,
          ready: rm.get(node) === 1,
        };
      })
      .sort((a, b) => a.node.localeCompare(b.node));
  },
  retry: false,
  refetchInterval: 60_000,
});

const curves = useQuery({
  queryKey: ["cluster", p.id, "curves"],
  queryFn: async () => {
    const end = Math.floor(Date.now() / 1000);
    const start = end - 24 * 3600;
    const [cpu, mem] = await Promise.all([
      prom.range('100 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))*100', {
        start,
        end,
        step: 900,
      }),
      prom.range(
        "100 * (1 - sum(node_memory_MemAvailable_bytes)/sum(node_memory_MemTotal_bytes))",
        { start, end, step: 900 },
      ),
    ]);
    const pts = cpu.data?.result?.[0]?.values ?? [];
    return {
      labels: pts.map((v) => {
        const d = new Date(Number(v[0]) * 1000);
        return `${String(d.getHours()).padStart(2, "0")}:00`;
      }),
      series: [
        { name: "CPU %", data: pts.map((v) => Number(v[1])) },
        {
          name: t("cd.memPercent"),
          data: (mem.data?.result?.[0]?.values ?? []).map((v) => Number(v[1])),
        },
      ],
    };
  },
  retry: false,
});

const pageActions = computed(() => [
  {
    label: t("cd.sync"),
    run: async () => {
      await jobs.list({ refresh: true });
      await all.refetch();
      notify(t("cd.synced"), "ok");
    },
  },
  {
    label: t("cd.credential"),
    run: async () => {
      try {
        cred.value = await admin.credential(p.id);
        credOpen.value = true;
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("cd.openUi"),
    // 集群级没有单一 JobManager：跳到作业列表让用户选具体作业
    run: () => router.push("/jobs"),
  },
]);

const jobCols = computed(() => [
  { key: "name", label: t("cd.col.name") },
  { key: "state", label: t("cd.col.state") },
  { key: "parallelism", label: t("cd.col.parallelism"), align: "right" as const },
  { key: "exposed", label: "REST" },
]);
const nsCols = computed(() => [
  { key: "ns", label: t("cd.col.ns") },
  { key: "jobs", label: t("cd.col.jobs"), align: "right" as const },
  { key: "slots", label: t("cd.col.slots"), align: "right" as const },
  { key: "green", label: t("cd.col.healthy"), align: "right" as const },
  { key: "nodeport", label: "NodePort", align: "right" as const },
]);
</script>

<template>
  <PageHead
    :title="p.id"
    :sub="t('cd.sub')"
    :badge="t('cd.badgeJobs', { n: mine.length })"
    tone="ok"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))">
    <Kpi :label="t('cd.kpi.jobs')" :value="mine.length" />
    <Kpi :label="t('cd.kpi.ns')" :value="byNs.length" />
    <Kpi
      :label="t('cd.kpi.health')"
      :value="mine.filter((j) => j.health === 'green').length"
      tone="ok"
    />
    <Kpi
      label="NodePort"
      :value="mine.filter((j) => j.restExposed === 'NodePort').length"
      tone="warn"
    />
  </div>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'overview', label: t('cd.tab.overview') },
      { key: 'ns', label: t('cd.tab.ns'), badge: byNs.length },
      { key: 'jobs', label: t('cd.tab.jobs'), badge: mine.length },
      { key: 'nodes', label: t('cd.tab.nodes') },
      { key: 'events', label: t('cd.tab.events') },
    ]"
  />

  <Card v-if="tab === 'overview'" :title="t('cd.distCard')">
    <div
      v-for="n in byNs"
      :key="n.ns"
      class="flex items-center gap-3 border-b border-line-2/50 py-1.5 text-xs last:border-0"
    >
      <span class="w-44 font-mono text-fg-1">{{ n.ns }}</span>
      <Badge tone="idle">{{ t("cd.nsJobs", { n: n.jobs }) }}</Badge>
      <span class="text-ok">{{ t("cd.nsHealthy", { n: n.green }) }}</span>
      <span v-if="n.nodeport" class="text-warn">{{ n.nodeport }} NodePort</span>
    </div>
    <div v-if="!byNs.length" class="py-4 text-center text-xs text-fg-2">{{ t("cd.noJobs") }}</div>
  </Card>

  <Card
    v-if="tab === 'overview'"
    :title="t('cd.cpuMemCard')"
    :hint="t('cd.cpuMemHint')"
    class="mt-3"
  >
    <LineChart
      :series="curves.data?.value?.series ?? []"
      :labels="curves.data?.value?.labels ?? []"
      :height="170"
      :y-fmt="(v: number) => v.toFixed(0) + '%'"
      :empty="t('cd.promDown')"
    />
  </Card>

  <DataTable
    v-else-if="tab === 'jobs'"
    :columns="jobCols"
    :rows="mine"
    :page-size="20"
    :empty="t('cd.emptyJobs')"
  >
    <template #cell-name="{ row }">
      <RouterLink
        :to="`/jobs/${row.cluster}/${row.namespace}/${row.name}`"
        class="font-medium hover:text-primary"
        >{{ row.name }}</RouterLink
      >
      <div class="font-mono text-3xs text-fg-2">{{ row.namespace }}</div>
    </template>
    <template #cell-state="{ row }">
      <Badge :tone="row.health === 'green' ? 'ok' : row.health === 'red' ? 'danger' : 'warn'" dot>{{
        row.state
      }}</Badge>
    </template>
    <template #cell-exposed="{ row }">
      <span :class="row.restExposed === 'NodePort' ? 'text-warn' : 'text-fg-2'">{{
        row.restExposed ?? "—"
      }}</span>
    </template>
  </DataTable>

  <DataTable
    v-else-if="tab === 'ns'"
    :columns="nsCols"
    :rows="byNs"
    row-key="ns"
    :empty="t('cd.emptyNs')"
  />

  <Card
    v-else-if="tab === 'nodes'"
    :title="t('cd.col.node')"
    :hint="t('cd.nodesHint', { n: nodeRows.data?.value?.length ?? 0 })"
    :pad="false"
  >
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("cd.nodesCard") }}</th>
            <th class="px-3 py-2">{{ t("cd.col.role") }}</th>
            <th class="px-3 py-2">{{ t("cd.col.state") }}</th>
            <th class="px-3 py-2 text-right">{{ t("cd.col.cpuAlloc") }}</th>
            <th class="px-3 py-2 text-right">{{ t("cd.col.memAlloc") }}</th>
            <th class="px-3 py-2 text-right">Pod</th>
            <th class="px-3 py-2 text-right">Flink Pod</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="n in nodeRows.data?.value ?? []"
            :key="n.node"
            class="border-b border-line-2/60 last:border-0"
          >
            <td class="px-3 py-1.5 font-mono">{{ n.node }}</td>
            <td class="px-3 py-1.5">
              <span
                class="rounded border border-line px-1 py-0.5 text-3xs"
                :class="n.role === 'worker' ? 'text-fg-2' : 'text-warn'"
                >{{ n.role }}</span
              >
            </td>
            <td class="px-3 py-1.5">
              <span
                class="rounded-full px-1.5 py-0.5 text-3xs"
                :class="n.ready ? 'bg-ok/10 text-ok' : 'bg-danger/10 text-danger'"
                >{{ n.ready ? "Ready" : "NotReady" }}</span
              >
            </td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(n.cpu) ? n.cpu : "—" }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(n.memGiB) ? n.memGiB.toFixed(0) + " GiB" : "—" }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">
              {{ Number.isFinite(n.pods) ? n.pods : "—" }}
            </td>
            <td class="px-3 py-1.5 text-right font-mono">{{ n.flinkPods }}</td>
          </tr>
          <tr v-if="!(nodeRows.data?.value ?? []).length">
            <td colspan="7" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("cd.noNodeInfo") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>

  <EmptyState v-else :title="t('cd.eventsCard')" :desc="t('cd.eventsDesc')" />

  <Modal v-model="credOpen" :title="t('cd.accessCard')" :sub="p.id" width="600px">
    <div v-if="cred" class="flex flex-col gap-1.5 text-xs">
      <div class="flex justify-between">
        <span class="text-fg-2">api-server</span
        ><span class="font-mono">{{ cred.apiServer ?? "—" }}</span>
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("cd.accessMode") }}</span><span class="font-mono">{{
          cred.inCluster ? "in-cluster" : "kubeconfig"
        }}</span>
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("cd.readonly") }}</span><span>{{
          cred.readonly ? "是" : "否"
        }}</span>
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("cd.jobCount") }}</span><span class="font-mono">{{
          cred.jobCount
        }}</span>
      </div>
      <p class="mt-2 rounded-lg bg-surface-2 px-3 py-2 text-2xs text-fg-2">{{ cred.note }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="credOpen = false"
      >
        {{ t("cd.close") }}
      </button>
    </template>
  </Modal>
</template>
