<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import Kpi from "../components/Kpi.vue";
import DataTable from "../components/DataTable.vue";
import EmptyState from "../components/EmptyState.vue";
import Badge from "../components/Badge.vue";
import { jobs, sessions } from "../api/client";
import Card from "../components/Card.vue";
import { ref } from "vue";
import { t } from "../lib/i18n";

const q = useQuery({
  queryKey: ["sessions"],
  queryFn: () => sessions.list(),
  refetchInterval: 30_000,
});
const items = computed(() => q.data?.value?.items ?? []);
const slots = computed(() =>
  items.value.reduce((n, s) => n + s.taskManagerReplicas * s.slotsPerTm, 0),
);

const jobsQ = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list() });
const namespaces = computed(() => {
  const s = new Set<string>();
  for (const j of jobsQ.data?.value?.items ?? []) s.add(j.namespace);
  return [...s].sort();
});

const form = ref({ name: "", ns: "", slots: 4, tm: 2, version: "", image: "" });
const msg = ref("");
const busy = ref(false);

async function create(dryRun: boolean) {
  if (!form.value.name.trim()) {
    msg.value = t("sess.needName");
    return;
  }
  if (
    !dryRun &&
    !(await confirmDialog({ message: t("sess.createConfirm", { name: form.value.name }) }))
  )
    return;
  busy.value = true;
  msg.value = "";
  try {
    const r = await sessions.create({
      name: form.value.name,
      namespace: form.value.ns || namespaces.value[0] || undefined,
      slots: form.value.slots,
      taskManagerReplicas: form.value.tm,
      flinkVersion: form.value.version.trim() || undefined,
      image: form.value.image.trim() || undefined,
      dryRun,
    });
    msg.value = dryRun
      ? t("sess.precheckOk", { ns: r.namespace, name: r.name })
      : t("sess.created", { ns: r.namespace, name: r.name });
    if (!dryRun) await q.refetch();
  } catch (e) {
    msg.value = t("sess.failed", { err: (e as Error).message });
  } finally {
    busy.value = false;
  }
}

const cols = computed(() => [
  { key: "name", label: t("sess.col.cluster") },
  { key: "ver", label: t("sess.col.versionImage") },
  { key: "jm", label: "JobManager" },
  { key: "tm", label: t("sess.col.tmPool"), align: "right" as const },
  { key: "slotUse", label: t("sess.col.slotUse"), align: "right" as const },
  { key: "sql", label: t("sess.col.sql") },
  { key: "ttl", label: t("sess.col.ttl") },
  { key: "state", label: t("sess.col.state") },
  { key: "ui", label: "", align: "right" as const },
]);
const rows = computed(() =>
  items.value.map((s) => ({
    ...s,
    jm: `${s.jobManagerCpu ?? "—"}C / ${s.jobManagerMemory ?? "—"}`,
    tm: `${s.taskManagerReplicas} × ${s.slotsPerTm}`,
    ver: `${s.flinkVersion ?? "—"} · ${
      String(s.image ?? "")
        .split("/")
        .pop() ?? "—"
    }`,
    slotUse: `${s.usedSlots ?? "—"} / ${s.taskManagerReplicas * s.slotsPerTm}`,
    sql: s.sqlGateway ? t("sess.deployed") : "—",
    ttl: s.idleTtl ? String(s.idleTtl) : t("sess.unset"),
  })),
);

// 空闲槽位与容量建议
const totalSlots = computed(() =>
  items.value.reduce((n, s) => n + s.taskManagerReplicas * s.slotsPerTm, 0),
);
const usedSlots = computed(() => items.value.reduce((n, s) => n + (s.usedSlots ?? 0), 0));
const idleSlots = computed(() => Math.max(0, totalSlots.value - usedSlots.value));
const sessionJobs = computed(() =>
  (jobsQ.data?.value?.items ?? []).filter((j) => j.kind === "FlinkSessionJob"),
);

const suggestions = computed(() =>
  items.value
    .map((s) => {
      const cap = s.taskManagerReplicas * s.slotsPerTm;
      const used = s.usedSlots ?? 0;
      const idle = cap - used;
      const kind = idle >= cap ? "reclaim" : idle > 0 ? "idle" : "full";
      const level = t(`sess.${kind === "reclaim" ? "reclaimable" : kind}`);
      const advice =
        kind === "reclaim"
          ? t("sess.advice.reclaim", { n: cap })
          : kind === "idle"
            ? t("sess.advice.idle", { n: idle })
            : t("sess.advice.full");
      return { name: s.name, cap, used, idle, level, kind, advice };
    })
    .sort((a, b) => b.idle - a.idle),
);
</script>

<template>
  <PageHead
    :title="t('page.sessions')"
    :sub="t('sess.sub', { n: items.length, slots })"
    :badge="t('sess.badge')"
    tone="info"
  />
  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(180px, 1fr))">
    <Kpi :label="t('sess.kpi.sessions')" :value="items.length" />
    <Kpi
      :label="t('sess.kpi.slots')"
      :value="`${usedSlots} / ${totalSlots}`"
      :foot="t('sess.idleFoot', { n: idleSlots })"
    />
    <Kpi
      label="SQL Gateway"
      :value="items.filter((s) => s.sqlGateway).length"
      :foot="t('sess.gatewayFoot')"
    />
    <Kpi label="SessionJob" :value="sessionJobs.length" :foot="t('sess.jobsFoot')" />
  </div>

  <DataTable v-if="items.length" :columns="cols" :rows="rows" row-key="name">
    <template #cell-name="{ row }">
      <span class="font-medium">{{ row.name }}</span>
      <div class="font-mono text-3xs text-fg-2">{{ row.cluster }} / {{ row.namespace }}</div>
    </template>
    <template #cell-state="{ row }"
      ><Badge :tone="row.state === 'RUNNING' ? 'ok' : 'warn'" dot>{{ row.state }}</Badge></template
    >
    <template #cell-ui="{ row }">
      <a
        :href="`/ui/${row.cluster}/${row.namespace}/${row.name}`"
        target="_blank"
        class="text-xs text-primary hover:underline"
        >Flink UI</a
      >
    </template>
  </DataTable>

  <EmptyState v-else :title="t('sess.emptyTitle')" :desc="t('sess.emptyDesc')" />

  <div class="mt-3 grid gap-3 md:grid-cols-2">
    <Card
      :title="t('sess.jobsCard')"
      :hint="t('sess.jobsHint', { n: sessionJobs.length })"
      :pad="false"
    >
      <div class="divide-y divide-line-2/70">
        <div v-for="j in sessionJobs" :key="j.name" class="flex items-center gap-2 px-4 py-2">
          <span class="truncate font-mono text-2xs">{{ j.name }}</span>
          <Badge class="ml-auto" :tone="j.state === 'RUNNING' ? 'ok' : 'warn'">{{ j.state }}</Badge>
        </div>
        <div v-if="!sessionJobs.length" class="px-4 py-6 text-center text-xs text-fg-2">
          {{ t("sess.noSessionJobs") }}
        </div>
      </div>
    </Card>

    <Card :title="t('sess.capacityCard')" :hint="t('sess.capacityHint')" :pad="false">
      <div class="divide-y divide-line-2/70">
        <div v-for="s in suggestions" :key="s.name" class="px-4 py-2">
          <div class="flex items-center gap-2">
            <span class="truncate font-mono text-2xs">{{ s.name }}</span>
            <span
              class="ml-auto rounded-full px-1.5 py-0.5 text-3xs"
              :class="
                s.kind === 'reclaim'
                  ? 'bg-danger/10 text-danger'
                  : s.kind === 'idle'
                    ? 'bg-warn/10 text-warn'
                    : 'bg-ok/10 text-ok'
              "
              >{{ s.level }}</span
            >
          </div>
          <div class="mt-0.5 text-3xs text-fg-2">
            {{ t("sess.slotLine", { used: s.used, cap: s.cap, advice: s.advice }) }}
          </div>
        </div>
        <div v-if="!suggestions.length" class="px-4 py-6 text-center text-xs text-fg-2">
          {{ t("sess.noSessions") }}
        </div>
      </div>
    </Card>
  </div>

  <Card :title="t('sess.createCard')" :hint="t('sess.createHint')" class="mt-3">
    <div class="flex flex-wrap items-end gap-2 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.name") }}</span>
        <input
          v-model="form.name"
          placeholder="flink-sql-session"
          class="w-48 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.ns") }}</span>
        <select v-model="form.ns" class="w-40 rounded-lg border border-line bg-surface-2 px-2 py-1">
          <option value="">{{ namespaces[0] ?? t("sess.noNs") }}</option>
          <option v-for="n in namespaces" :key="n" :value="n">{{ n }}</option>
        </select></label
      >
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.flinkVersion") }}</span>
        <input
          v-model="form.version"
          placeholder="v1_18"
          class="w-28 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.image") }}</span>
        <input
          v-model="form.image"
          placeholder="harbor/flink:1.18.1"
          class="w-64 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.slotsPerTm") }}</span>
        <input
          v-model.number="form.slots"
          type="number"
          min="1"
          class="w-24 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sess.tmReplicas") }}</span>
        <input
          v-model.number="form.tm"
          type="number"
          min="1"
          class="w-24 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
      /></label>
      <button
        :disabled="busy"
        class="rounded-lg bg-surface-3 px-3 py-1 hover:bg-hover disabled:opacity-50"
        @click="create(true)"
      >
        {{ t("sess.precheck") }}
      </button>
      <button
        :disabled="busy"
        class="rounded-lg bg-primary px-3 py-1 font-medium text-white disabled:opacity-50"
        @click="create(false)"
      >
        {{ t("sess.create") }}
      </button>
      <span v-if="msg" class="text-fg-2">{{ msg }}</span>
    </div>
    <p class="mt-2 text-2xs text-fg-2">
      {{ t("sess.notePrefix") }}
      <code class="font-mono">mode=native</code>{{ t("sess.noteMode")
      }}<strong class="text-fg">{{ t("sess.noteNoJob") }}</strong
      >{{ t("sess.noteJoin") }}
      {{ t("sess.noteTail") }}
      <code class="font-mono">mode=session</code>{{ t("sess.noteEnd") }}
    </p>
  </Card>
</template>
