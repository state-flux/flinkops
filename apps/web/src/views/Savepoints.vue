<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { useRouter } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Badge from "../components/Badge.vue";
import Donut from "../components/charts/Donut.vue";
import { jobs, snapshots, snapshotsApi } from "../api/client";
import { notify } from "../lib/notify";
import { useWritable } from "../lib/session";
import type { SnapshotItem } from "../api/types";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

const router = useRouter();
const picked = ref("");
const msg = ref("");
const busy = ref(false);
const filterState = ref("");

const list = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 60_000 });
const items = computed(() => list.data?.value?.items ?? []);
const target = computed(() =>
  items.value.find((j) => `${j.cluster}/${j.namespace}/${j.name}` === picked.value),
);
const { readonly } = useWritable();

const snaps = useQuery({
  queryKey: ["snapshots", "all"],
  queryFn: () => snapshots.list(),
  refetchInterval: 30_000,
  retry: false,
});
const snapRows = computed(() => {
  const all = snaps.data?.value?.items ?? [];
  return filterState.value ? all.filter((s) => s.state === filterState.value) : all;
});

const detail = useQuery({
  queryKey: ["job", "sp", picked],
  queryFn: () => jobs.detail(target.value!.cluster, target.value!.namespace, target.value!.name),
  enabled: computed(() => !!target.value),
  retry: false,
});

const sp = computed(
  () =>
    ((detail.data?.value as Record<string, unknown>)?.savepoint ?? {}) as {
      history?: { id?: string; path?: string; timestamp?: number; state?: string }[];
      last?: { id?: string; path?: string };
    },
);
const ck = computed(
  () =>
    ((detail.data?.value as Record<string, unknown>)?.checkpoint ?? {}) as {
      completed?: number;
      failed?: number;
      last?: { durationMs?: number; size?: number };
    },
);
const cfg = computed(() => {
  const spec = (detail.data?.value as Record<string, unknown>)?.spec as
    { flinkConfiguration?: Record<string, string> } | undefined;
  return spec?.flinkConfiguration ?? {};
});

const counts = computed(() => {
  const all = snaps.data?.value?.items ?? [];
  return {
    total: all.length,
    ok: all.filter((s) => s.state === "COMPLETED").length,
    bad: all.filter((s) => ["FAILED", "ABANDONED"].includes(String(s.state))).length,
    running: all.filter((s) => s.state === "IN_PROGRESS").length,
  };
});

// 存储分布：CR 里只有 path，没有体积；按作业分组给出快照数量分布
const byJob = computed(() => {
  const m = new Map<string, number>();
  for (const s of snapRows.value) m.set(s.jobName || "—", (m.get(s.jobName || "—") ?? 0) + 1);
  return [...m.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 6)
    .map(([label, value]) => ({ label, value }));
});

// 后端是 UTC，这里按本地时区显示；解析不了给占位符
const fmtTs = (v?: number | string) => fmtLocal(v);
const rel = (v?: number | string) => {
  if (!v) return "—";
  const ts = typeof v === "number" ? v : new Date(v).getTime();
  const d = (Date.now() - ts) / 1000;
  if (!Number.isFinite(d)) return "—";
  return d < 3600
    ? t("sp.agoMin", { n: Math.round(d / 60) })
    : d < 86400
      ? t("sp.agoHour", { n: Math.round(d / 3600) })
      : t("sp.agoDay", { n: Math.round(d / 86400) });
};

async function trigger() {
  if (!target.value) return;
  if (!(await confirmDialog({ message: t("sp.triggerConfirm", { name: target.value.name }) })))
    return;
  busy.value = true;
  msg.value = "";
  try {
    const r = await jobs.savepoint(target.value.cluster, target.value.namespace, target.value.name);
    msg.value =
      r.mode === "rest"
        ? t("sp.degraded", { id: r.requestId ?? "" })
        : t("sp.submitted", { id: r.snapshot ?? "" });
    notify(msg.value, r.mode === "rest" ? "warn" : "ok");
    await snaps.refetch();
  } catch (e) {
    msg.value = t("sp.failedMsg", { err: (e as Error).message });
    notify(msg.value, "danger");
  } finally {
    busy.value = false;
  }
}

async function doRestore(path?: string) {
  if (!target.value) return;
  const from = path || t("sp.latestSavepoint");
  if (
    !(await confirmDialog({
      message: t("sp.restoreConfirm", { from, name: target.value.name }),
    }))
  )
    return;
  try {
    const r = await jobs.restore(
      target.value.cluster,
      target.value.namespace,
      target.value.name,
      path,
    );
    notify(t("sp.restoreDone", { from: r.from ?? from }), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function purgeFailed() {
  // 只清理当前筛选下可见的 FAILED（原来是全量扫描，跨集群跨命名空间一起删）
  const failed = snapRows.value.filter((s) => ["FAILED", "ABANDONED"].includes(String(s.state)));
  if (!failed.length) {
    notify(t("sp.noFailed"), "warn");
    return;
  }
  const preview = failed
    .slice(0, 6)
    .map((s) => `· ${s.jobName ?? "?"} / ${s.namespace} / ${s.name}`)
    .join("\n");
  const more = failed.length > 6 ? t("sp.more", { n: failed.length }) : "";
  if (
    !(await confirmDialog({
      message: t("sp.deleteFailedConfirm", { preview, more }),
      danger: true,
    }))
  )
    return;
  let ok = 0;
  const errs: string[] = [];
  for (const s of failed) {
    try {
      await snapshotsApi.remove(s.cluster, s.namespace, s.name);
      ok += 1;
    } catch (e) {
      errs.push(`${s.name}: ${(e as Error).message}`);
    }
  }
  notify(
    t("sp.deletedLine", {
      ok,
      total: failed.length,
      errs: errs.length ? t("sp.deletedErrsDetail", { n: errs.length, first: errs[0] ?? "" }) : "",
    }),
    ok ? "ok" : "danger",
    errs.length ? 8000 : 3200,
  );
  await snaps.refetch();
}

// 从列表里某一行的快照恢复（不需要先在左侧选作业）
async function doRestoreFrom(s: SnapshotItem) {
  const target = `${s.jobName} @ ${s.path}`;
  if (
    !(await confirmDialog({
      message: t("sp.restoreFromConfirm", {
        cluster: String(s.cluster ?? ""),
        ns: String(s.namespace ?? ""),
        job: String(s.jobName ?? ""),
        path: s.path ?? "",
      }),
    }))
  )
    return;
  try {
    const r = await jobs.restore(s.cluster ?? "", s.namespace ?? "", s.jobName ?? "", s.path);
    notify(t("sp.restoreSubmitted", { from: r.from ?? target }), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

const pageActions = computed(() => [
  { label: t("sp.cleanupFailed"), run: () => void purgeFailed() },
  {
    label: t("sp.restore"),
    run: () => (target.value ? void doRestore() : notify(t("sp.pickJobFirst"), "warn")),
  },
  {
    label: t("sp.logs"),
    run: () =>
      target.value
        ? void router.push(
            `/jobs/${target.value.cluster}/${target.value.namespace}/${target.value.name}`,
          )
        : notify(t("sp.pickJobFirst"), "warn"),
  },
]);
</script>

<template>
  <PageHead
    :title="t('page.savepoints')"
    :sub="t('sp.sub', { snapshots: counts.total, jobs: items.length })"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <!-- KPI -->
  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))">
    <Kpi
      :label="t('sp.kpi.total')"
      :value="counts.total"
      :foot="`${counts.ok} 完成 · ${counts.bad} 失败`"
    />
    <Kpi :label="t('sp.kpi.size')" value="—" foot="需 HDFS 侧统计（平台未接 WebHDFS）" />
    <Kpi
      :label="t('sp.kpi.running')"
      :value="counts.running"
      :tone="counts.running ? 'warn' : 'fg'"
    />
    <Kpi :label="t('sp.kpi.failed')" :value="counts.bad" :tone="counts.bad ? 'danger' : 'ok'" />
  </div>

  <!-- 触发 + 存储分布 -->
  <div class="mb-3 grid gap-3 xl:grid-cols-[1.5fr_1fr]">
    <Card :title="t('sp.triggerCard')" :hint="t('sp.triggerHint')">
      <div class="flex flex-wrap items-center gap-2">
        <select
          v-model="picked"
          class="min-w-[260px] rounded-lg border border-line bg-surface-2 px-2 py-1 text-xs"
        >
          <option value="">{{ t("sp.pickJob") }}</option>
          <option
            v-for="j in items"
            :key="`${j.cluster}/${j.namespace}/${j.name}`"
            :value="`${j.cluster}/${j.namespace}/${j.name}`"
          >
            {{ j.namespace }} / {{ j.name }}
          </option>
        </select>
        <button
          :disabled="readonly || !target || busy"
          class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
          @click="trigger"
        >
          {{ busy ? t("sp.submitting") : t("sp.triggerCard") }}
        </button>
        <button
          :disabled="readonly || !target"
          class="rounded-lg border border-line px-3 py-1 text-xs disabled:opacity-50"
          @click="doRestore()"
        >
          {{ t("sp.restoreLatest") }}
        </button>
      </div>
      <p v-if="msg" class="mt-2 text-2xs text-fg-2">{{ msg }}</p>

      <div
        v-if="target"
        class="mt-3 grid gap-2"
        style="grid-template-columns: repeat(auto-fit, minmax(130px, 1fr))"
      >
        <Kpi :label="t('sp.history')" :value="(sp.history ?? []).length" />
        <Kpi :label="t('sp.lastId')" :value="sp.last?.id?.slice(0, 12) ?? '—'" />
        <Kpi :label="t('sp.ckptOk')" :value="ck.completed ?? 0" tone="ok" />
        <Kpi :label="t('sp.ckptBad')" :value="ck.failed ?? 0" :tone="ck.failed ? 'danger' : 'fg'" />
      </div>
      <div v-if="target" class="mt-2 flex flex-col gap-1">
        <div
          v-for="(h, i) in sp.history ?? []"
          :key="i"
          class="flex items-center gap-2 border-b border-line-2/50 py-1 text-2xs last:border-0"
        >
          <span class="font-mono text-fg-2">#{{ h.id ?? i }}</span>
          <span class="truncate font-mono text-fg-2" :title="h.path">{{ h.path ?? "—" }}</span>
          <span class="ml-auto shrink-0 text-3xs text-fg-2">{{ rel(h.timestamp) }}</span>
        </div>
        <div v-if="!(sp.history ?? []).length" class="py-3 text-center text-2xs text-fg-2">
          {{ t("sp.noHistory") }}
        </div>
      </div>
    </Card>

    <Card :title="t('sp.distCard')" :hint="t('sp.distHint')">
      <Donut
        :items="byJob"
        :size="118"
        :center-label="t('sp.col.snapshot')"
        :center-value="String(counts.total)"
      />
      <p class="mt-2 text-3xs leading-relaxed text-fg-2">
        {{ t("sp.sizeNotePrefix") }}
        <span class="font-mono">spec.savepoint.path</span>{{ t("sp.sizeNoteTail") }}
      </p>
    </Card>
  </div>

  <!-- 保留策略 + 定时 savepoint -->
  <div class="mb-3 grid gap-3 md:grid-cols-2">
    <Card :title="t('sp.retentionCard')" :hint="t('sp.retentionHint')">
      <div v-if="target" class="flex flex-col gap-1.5 text-2xs">
        <div class="flex justify-between">
          <span class="text-fg-2">history.max.count</span
          ><span class="font-mono">{{
            cfg["kubernetes.operator.savepoint.history.max.count"] ?? t("sp.unsetDefault10")
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">history.max.age</span
          ><span class="font-mono">{{
            cfg["kubernetes.operator.savepoint.history.max.age"] ?? t("sp.unsetDefault168h")
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">savepoints.dir</span
          ><span class="truncate font-mono text-3xs" :title="cfg['state.savepoints.dir']">{{
            cfg["state.savepoints.dir"] ?? "—"
          }}</span>
        </div>
        <div class="flex justify-between">
          <span class="text-fg-2">checkpoints.num-retained</span
          ><span class="font-mono">{{
            cfg["state.checkpoints.num-retained"] ?? t("sp.unsetDefault1")
          }}</span>
        </div>
      </div>
      <p v-else class="text-xs text-fg-2">{{ t("sp.retentionEmpty") }}</p>
    </Card>

    <Card :title="t('sp.timedCard')" hint="operator periodic savepoint">
      <div v-if="target" class="flex flex-col gap-1.5 text-2xs">
        <div class="flex justify-between">
          <span class="text-fg-2">periodic.savepoint.interval</span
          ><span class="font-mono">{{
            cfg["kubernetes.operator.periodic.savepoint.interval"] ?? t("sp.unset")
          }}</span>
        </div>
        <p class="mt-2 text-3xs leading-relaxed text-fg-2">
          {{ t("sp.timedNoteTail") }}
          {{
            cfg["kubernetes.operator.periodic.savepoint.interval"]
              ? t("sp.timedOn")
              : t("sp.timedOff")
          }}
        </p>
      </div>
      <p v-else class="text-xs text-fg-2">{{ t("sp.timedEmpty") }}</p>
    </Card>
  </div>

  <!-- 快照列表 -->
  <Card :title="t('sp.allSnapshots')" :pad="false">
    <template #actions>
      <div class="inline-flex overflow-hidden rounded-lg border border-line text-2xs">
        <button
          v-for="s in [
            { k: '', t: t('sp.range.all') },
            { k: 'COMPLETED', t: t('sp.range.completed') },
            { k: 'IN_PROGRESS', t: t('sp.range.running') },
            { k: 'FAILED', t: t('sp.range.failed') },
          ]"
          :key="s.k"
          class="px-2 py-0.5"
          :class="
            filterState === s.k ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'
          "
          @click="filterState = s.k"
        >
          {{ s.t }}
        </button>
      </div>
    </template>
    <p
      v-if="(snaps.data?.value?.unsupportedClusters ?? []).length"
      class="mx-4 mt-3 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-2xs text-warn"
    >
      {{
        t("sp.noCrdClusters", { list: (snaps.data?.value?.unsupportedClusters ?? []).join(", ") })
      }}
    </p>
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("sp.col.snapshot") }}</th>
            <th class="px-3 py-2">{{ t("sp.col.job") }}</th>
            <th class="px-3 py-2">{{ t("sp.col.state") }}</th>
            <th class="px-3 py-2">{{ t("sp.col.path") }}</th>
            <th class="px-3 py-2">{{ t("sp.col.createdAt") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="s in snapRows" :key="s.name" class="border-b border-line-2/60 last:border-0">
            <td class="px-3 py-1.5">
              <div class="font-mono">{{ s.name }}</div>
              <div class="font-mono text-3xs text-fg-2">{{ s.cluster }}/{{ s.namespace }}</div>
            </td>
            <td class="px-3 py-1.5 font-mono text-2xs">
              <RouterLink
                :to="`/jobs/${s.cluster}/${s.namespace}/${s.jobName}`"
                class="hover:text-primary"
                >{{ s.jobName }}</RouterLink
              >
            </td>
            <td class="px-3 py-1.5">
              <Badge
                :tone="
                  s.state === 'COMPLETED'
                    ? 'ok'
                    : s.state === 'FAILED' || s.state === 'ABANDONED'
                      ? 'danger'
                      : s.state === 'IN_PROGRESS'
                        ? 'warn'
                        : 'idle'
                "
                >{{ s.state }}</Badge
              >
            </td>
            <td
              class="max-w-[300px] truncate px-3 py-1.5 font-mono text-3xs text-fg-2"
              :title="String(s.path ?? '')"
            >
              {{ s.path ?? "—" }}
            </td>
            <td class="whitespace-nowrap px-3 py-1.5 font-mono text-3xs text-fg-2">
              {{ fmtTs(s.createdAt) }}
            </td>
            <td class="whitespace-nowrap px-3 py-1.5 text-right">
              <button
                v-if="s.path && s.state === 'COMPLETED'"
                class="rounded border border-line px-1.5 py-0.5 text-3xs hover:border-primary/50"
                @click="doRestoreFrom(s)"
              >
                {{ t("sp.restoreFromThis") }}
              </button>
            </td>
          </tr>
          <tr v-if="!snapRows.length">
            <td colspan="6" class="px-3 py-8 text-center text-xs text-fg-2">{{ t("sp.empty") }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>
</template>
