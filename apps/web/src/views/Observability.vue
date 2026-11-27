<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Badge from "../components/Badge.vue";
import DataTable from "../components/DataTable.vue";
import { auth, jobs, observability, prom } from "../api/client";
import { notify } from "../lib/notify";
import { t } from "../lib/i18n";

const tab = ref("o-list");
const ns = ref("flink");
const interval = ref("30s");

const me = useQuery({ queryKey: ["me"], queryFn: auth.me });
const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 60_000 });
const items = computed(() => all.data?.value?.items ?? []);

// 三处对齐检查：reporter 配置 / containerPort 名 / scrape 标签
const rows = computed(() =>
  items.value.map((j) => ({
    ...j,
    reporter: j.restExposed === undefined ? "?" : t("obs.attr.hasPort"),
    nodeport: j.restExposed === "NodePort" ? t("obs.yes") : t("obs.no"),
  })),
);

// 注意：**不要**用一个 PodMonitor 同时选两个组件，除非你确定集群里没有旧的
// taskmanager-only PodMonitor —— 两个同时命中会让 TM 被抓两遍，sum() 直接翻倍。
// 这里生成两个 PM（各选一个组件），与已有 flink-taskmanagers 幂等共存。
const monitor = computed(
  () => `# 1) TaskManager（与已有 PodMonitor 同名，apply 是幂等的）
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-taskmanagers
  namespace: ${ns.value}
  labels:
    # 必须与 Prometheus CR 的 podMonitorSelector 匹配；Rancher monitoring 用这个
    release: rancher-monitoring
spec:
  selector:
    matchLabels:
      component: taskmanager
  podMetricsEndpoints:
    - port: prom-9250
      path: /metrics
      interval: ${interval.value}
  namespaceSelector:
    matchNames: ["${ns.value}"]
---
# 2) JobManager（checkpoint 时长/次数/flink_jobmanager_* 全靠它）
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-jobmanagers
  namespace: ${ns.value}
  labels:
    release: rancher-monitoring
spec:
  selector:
    matchLabels:
      component: jobmanager
  podMetricsEndpoints:
    - port: prom-9250
      path: /metrics
      interval: ${interval.value}
  namespaceSelector:
    matchNames: ["${ns.value}"]`,
);

// 单 PM 抓两组的写法：只在集群里**没有** taskmanager-only PM 时用，否则 TM 会被抓两遍
const monitorBoth = computed(
  () => `apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-jobs
  namespace: ${ns.value}
  labels:
    release: rancher-monitoring
spec:
  selector:
    matchExpressions:
      - key: component
        operator: In
        values: ["jobmanager", "taskmanager"]
  podMetricsEndpoints:
    - port: prom-9250
      path: /metrics
      interval: ${interval.value}
  namespaceSelector:
    matchNames: ["${ns.value}"]`,
);

const pageActions = computed(() => [
  {
    label: t("obs.recheck"),
    run: async () => {
      await all.refetch();
      notify(t("obs.rechecked"), "ok");
    },
  },
  {
    label: t("obs.genPodMonitor"),
    run: () => {
      tab.value = "o-gen";
      document.getElementById("pm-box")?.scrollIntoView({ behavior: "smooth" });
    },
  },
  {
    label: t("obs.patchAll"),
    run: async () => {
      if (
        !(await confirmDialog({
          message: t("obs.patchAllConfirm"),
        }))
      )
        return;
      try {
        const r = await observability.patchLabels({
          labels: { "flinkops.io/scrape": "true" },
          onlyNodeport: true,
        });
        notify(
          t("obs.patchAllDone", { ok: r.patched, total: r.total }) +
            (r.failed ? t("obs.patchAllFailed", { n: r.failed }) : ""),
          r.failed ? "warn" : "ok",
        );
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("obs.apply"),
    run: async () => {
      if (!(await confirmDialog({ message: t("obs.applyConfirm", { ns: ns.value }) }))) return;
      try {
        const r = await observability.applyPodMonitor(monitor.value);
        notify(
          t("obs.applyDone", { ns: r.namespace, name: r.name, hint: r.hint ?? "" }),
          "ok",
          8000,
        );
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("obs.copyYamlAction"),
    run: () => {
      void navigator.clipboard?.writeText(monitor.value);
      notify(t("obs.copied"), "ok");
    },
  },
]);

const cols = [
  { key: "name", label: t("obs.col.job") },
  { key: "ns", label: t("obs.col.ns") },
  { key: "state", label: t("obs.col.state") },
  { key: "nodeport", label: "NodePort" },
];

// 真实采集目标（Prometheus 的 up 序列），不是猜出来的
const targets = useQuery({
  queryKey: ["obs", "targets"],
  queryFn: async () => {
    const r = await prom.query('up{endpoint=~"prom-92.."}');
    return (r.data?.result ?? []).map((s) => {
      const pod = s.metric.pod ?? "—";
      // JM Pod 名形如 <job>-<rs hash>-<pod hash>，TM 名带 -taskmanager-<n>-<n>
      const isTm = pod.includes("-taskmanager-");
      const isJm = !isTm && /-[a-z0-9]{6,10}-[a-z0-9]{5}$/.test(pod);
      const component = isTm ? "taskmanager" : isJm ? "jobmanager" : "other";
      return {
        pod,
        ns: s.metric.namespace ?? "—",
        instance: s.metric.instance ?? "—",
        endpoint: s.metric.endpoint ?? "—",
        component,
        up: Number(s.value?.[1] ?? 0) === 1,
      };
    });
  },
  refetchInterval: 30_000,
  retry: false,
});
const tgtRows = computed(() => targets.data?.value ?? []);
const upCount = computed(() => tgtRows.value.filter((t) => t.up).length);
const jmTargets = computed(() => tgtRows.value.filter((t) => t.component === "jobmanager").length);
const tmTargets = computed(() => tgtRows.value.filter((t) => t.component === "taskmanager").length);
</script>

<template>
  <PageHead
    :title="t('page.observability')"
    :sub="t('obs.sub', { n: items.length })"
    :badge="me.data?.value?.prometheus ? t('obs.badgeOn') : t('obs.badgeOff')"
    :tone="me.data?.value?.prometheus ? 'ok' : 'warn'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))">
    <Kpi :label="t('obs.kpi.jobs')" :value="items.length" />
    <Kpi
      :label="t('obs.kpi.targets')"
      :value="`${upCount} / ${tgtRows.length}`"
      :foot="
        tgtRows.length ? t('obs.tmFoot', { jm: jmTargets, tm: tmTargets }) : t('obs.noTargetsFoot')
      "
      :tone="upCount === tgtRows.length && tgtRows.length ? 'ok' : 'warn'"
    />
    <Kpi
      :label="t('obs.kpi.jmTargets')"
      :value="jmTargets"
      :foot="jmTargets ? t('obs.jmOk') : t('obs.jmMissing')"
      :tone="jmTargets ? 'ok' : 'danger'"
    />
    <Kpi
      :label="t('obs.kpi.backend')"
      :value="me.data?.value?.prometheus ? 'Prometheus' : 'none'"
      :tone="me.data?.value?.prometheus ? 'ok' : 'warn'"
      foot="FLINKOPS_PROMETHEUS_URL"
    />
  </div>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'o-list', label: t('obs.tab.list') },
      { key: 'o-targets', label: t('obs.tab.targets', { n: tgtRows.length }) },
      { key: 'o-gen', label: t('obs.tab.gen') },
      { key: 'o-target', label: t('obs.tab.align') },
      { key: 'o-trouble', label: t('obs.tab.trouble') },
    ]"
  />

  <template v-if="tab === 'o-list'">
    <DataTable :columns="cols" :rows="rows" row-key="name" :empty="t('obs.empty')">
      <template #cell-name="{ row }"
        ><span class="font-medium">{{ row.name }}</span></template
      >
      <template #cell-state="{ row }"
        ><Badge :tone="row.health === 'green' ? 'ok' : 'warn'" dot>{{ row.state }}</Badge></template
      >
      <template #cell-nodeport="{ row }"
        ><span :class="row.nodeport === t('obs.yes') ? 'text-warn' : 'text-fg-2'">{{
          row.nodeport
        }}</span></template
      >
    </DataTable>
  </template>

  <template v-else-if="tab === 'o-targets'">
    <Card
      :title="t('obs.targetsCard')"
      :hint="`${upCount}/${tgtRows.length} up · JM ${jmTargets} / TM ${tmTargets}`"
      :pad="false"
    >
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">Pod</th>
              <th class="px-3 py-2">{{ t("obs.col.component") }}</th>
              <th class="px-3 py-2">{{ t("obs.col.endpoint") }}</th>
              <th class="px-3 py-2">{{ t("obs.col.instance") }}</th>
              <th class="px-3 py-2">{{ t("obs.col.state") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="t in tgtRows"
              :key="t.instance"
              class="border-b border-line-2/60 last:border-0"
            >
              <td class="max-w-[320px] truncate px-3 py-1.5 font-mono text-2xs" :title="t.pod">
                {{ t.pod }}
              </td>
              <td class="px-3 py-1.5">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs"
                  :class="
                    t.component === 'jobmanager'
                      ? 'bg-violet/10 text-violet'
                      : 'bg-info/10 text-info'
                  "
                  >{{ t.component }}</span
                >
              </td>
              <td class="px-3 py-1.5 font-mono text-3xs text-fg-2">{{ t.endpoint }}</td>
              <td class="px-3 py-1.5 font-mono text-3xs text-fg-2">{{ t.instance }}</td>
              <td class="px-3 py-1.5">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs"
                  :class="t.up ? 'bg-ok/10 text-ok' : 'bg-danger/10 text-danger'"
                  >{{ t.up ? "UP" : "DOWN" }}</span
                >
              </td>
            </tr>
            <tr v-if="!tgtRows.length">
              <td colspan="5" class="px-3 py-6 text-center text-xs text-fg-2">
                {{ t("obs.noTargets") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p
        v-if="tgtRows.length && !jmTargets"
        class="border-t border-line-2 px-3 py-2 text-2xs text-warn"
      >
        {{ t("obs.onlyTm")
        }}<span class="font-mono">Started PrometheusReporter HTTP server on port 9250</span
        >{{ t("obs.onlyTmTail") }}<span class="font-mono">component: taskmanager</span
        >{{ t("obs.onlyTmFix") }}
      </p>
    </Card>
  </template>

  <template v-else-if="tab === 'o-gen'">
    <Card id="pm-box" title="PodMonitor" :hint="t('obs.copyYaml')">
      <div class="mb-3 flex flex-wrap items-center gap-2 text-xs">
        <label class="flex items-center gap-1.5"
          >{{ t("obs.namespace")
          }}<input
            v-model="ns"
            class="w-32 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          />
        </label>
        <label class="flex items-center gap-1.5"
          >{{ t("obs.interval")
          }}<select v-model="interval" class="rounded-lg border border-line bg-surface-2 px-2 py-1">
            <option>15s</option>
            <option>30s</option>
            <option>60s</option>
          </select>
        </label>
      </div>
      <pre
        class="overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
        >{{ monitor }}</pre>
      <details class="mt-2 text-2xs text-fg-2">
        <summary class="cursor-pointer">
          {{ t("obs.onePmNote") }}
        </summary>
        <pre
          class="mt-1 overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
          >{{ monitorBoth }}</pre>
        <p class="mt-1">
          {{ t("obs.doubleSample") }}
          <span class="font-mono">sum(rate(...))</span> {{ t("obs.doubleSampleTail") }}
        </p>
      </details>
    </Card>
  </template>

  <template v-else-if="tab === 'o-target'">
    <Card :title="t('obs.align.title')" :hint="t('obs.align.hint')">
      <ol class="ml-4 flex list-decimal flex-col gap-2 text-xs leading-relaxed text-fg-1">
        <li>
          <b>{{ t("obs.align.jobSide") }}</b>：<code class="font-mono"
            >metrics.reporter.prom.factory.class</code
          >{{ t("obs.align.jobSideTail") }}<code class="font-mono">9250-9250</code
          >{{ t("obs.align.jobSideTail2") }}<b>{{ t("obs.align.jmTm") }}</b
          >{{ t("obs.align.samePort")
          }}<code class="font-mono">Started PrometheusReporter HTTP server on port 9250</code
          >{{ t("obs.align.rangeTail") }}<b>{{ t("obs.align.useActual") }}</b
          >{{ t("obs.align.twoEndpoints") }}
        </li>
        <li>
          <b>{{ t("obs.align.podSide") }}</b>：<code class="font-mono">podTemplate</code
          >{{ t("obs.align.podNeedsPort") }}<code class="font-mono">prom-9250</code
          >{{ t("obs.align.portNote") }}<code class="font-mono">spec.podTemplate</code
          >{{ t("obs.yes") }}<b>{{ t("obs.align.shared") }}</b
          >{{ t("obs.align.sharedTail")
          }}<code class="font-mono">Started PrometheusReporter HTTP server on port 9250</code>）。
        </li>
        <li>
          <b>{{ t("obs.align.platformSide") }}</b>{{ t("obs.align.selectorMust")
          }}<b>{{ t("obs.align.both") }}</b
          >{{ t("obs.align.onlyOne") }}<code class="font-mono">component: taskmanager</code
          >{{ t("obs.align.missBlock") }}<code class="font-mono">flink_jobmanager_*</code
          >{{ t("obs.align.missBlockTail") }}<code class="font-mono">metadata.labels.release</code
          >{{ t("obs.align.matchLabel") }}<code class="font-mono">podMonitorSelector</code
          >{{ t("obs.align.rancher")
          }}<code class="font-mono">release: rancher-monitoring</code>）。
        </li>
      </ol>
      <p class="mt-3 text-2xs text-fg-2">{{ t("obs.profileNote") }}</p>
    </Card>
  </template>

  <template v-else>
    <Card :title="t('obs.triage.title')">
      <ol class="ml-4 flex list-decimal flex-col gap-1.5 text-xs leading-relaxed text-fg-1">
        <li>{{ t("obs.triage.targets") }}<code class="font-mono">up</code>。</li>
        <li>
          {{ t("obs.triage.ifNot")
          }}<code class="font-mono">kubectl port-forward pod/&lt;tm-pod&gt; 9250</code
          >{{ t("obs.triage.then") }}<code class="font-mono">curl localhost:9250/metrics</code
          >{{ t("obs.triage.confirm") }}
        </li>
        <li>
          {{ t("obs.triage.selector")
          }}<code class="font-mono">kubectl get podmonitor -n {{ ns }} -o yaml</code
          >{{ t("obs.triage.selectorTail") }}
        </li>
        <li>{{ t("obs.triage.netpol") }}</li>
      </ol>
    </Card>
  </template>
</template>
