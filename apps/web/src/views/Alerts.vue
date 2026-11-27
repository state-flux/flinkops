<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Kpi from "../components/Kpi.vue";
import DataTable from "../components/DataTable.vue";
import Badge from "../components/Badge.vue";
import Modal from "../components/Modal.vue";
import { alerts, type AlertRule } from "../api/client";
import { notify } from "../lib/notify";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

const tab = ref("live");
const events = useQuery({
  queryKey: ["alerts", "events"],
  queryFn: () => alerts.events(),
  refetchInterval: 30_000,
});
const rules = useQuery({
  queryKey: ["alerts", "rules"],
  queryFn: alerts.rules,
  refetchInterval: 60_000,
});

const openEvents = computed(() =>
  (events.data?.value?.items ?? []).filter((e) => e.state === "OPEN"),
);
const acked = computed(() => (events.data?.value?.items ?? []).filter((e) => e.state === "ACK"));

const ruleOpen = ref(false);
const form = ref({ id: "", name: "", kind: "job_not_running", threshold: 0, severity: "warn" });

const kinds: { key: string; label: string; hint: string }[] = [
  { key: "job_not_running", label: t("al.rule.notRunning"), hint: t("al.rule.notRunningDesc") },
  { key: "restarts_high", label: t("al.rule.restarts"), hint: t("al.rule.restartsDesc") },
  {
    key: "nodeport_exposed",
    label: t("al.rule.nodeport"),
    hint: t("al.rule.nodeportDesc"),
  },
  { key: "slots_over", label: t("al.rule.slots"), hint: t("al.rule.slotsDesc") },
];

const pageActions = computed(() => [
  {
    label: t("al.refresh"),
    run: async () => {
      await events.refetch();
      await rules.refetch();
      notify(t("al.refreshed"), "ok");
    },
  },
  {
    label: t("al.newRule"),
    run: () => {
      form.value = { id: "", name: "", kind: "job_not_running", threshold: 0, severity: "warn" };
      ruleOpen.value = true;
    },
  },
  { label: t("al.ackAll"), run: ackAll },
]);

async function ackAll() {
  const list = openEvents.value;
  if (!list.length) {
    notify(t("al.noOpen"), "warn");
    return;
  }
  if (!(await confirmDialog({ message: t("al.ackConfirm", { n: list.length }) }))) return;
  let ok = 0;
  for (const e of list) {
    try {
      await alerts.ack(e.id);
      ok += 1;
    } catch {
      /* 继续 */
    }
  }
  notify(t("al.acked", { ok, total: list.length }), ok ? "ok" : "danger");
  await events.refetch();
}

async function ack(id: string) {
  try {
    await alerts.ack(id);
    await events.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function saveRule() {
  if (!form.value.name.trim()) {
    notify(t("al.needName"), "warn");
    return;
  }
  try {
    await alerts.saveRule({
      id: form.value.id || undefined,
      name: form.value.name.trim(),
      kind: form.value.kind,
      threshold: form.value.kind === "restarts_high" ? Number(form.value.threshold) : undefined,
      severity: form.value.severity,
    });
    notify(form.value.id ? t("al.ruleUpdated") : t("al.ruleSaved"), "ok");
    ruleOpen.value = false;
    await rules.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

function editRule(r: AlertRule) {
  form.value = {
    id: r.id,
    name: r.name,
    kind: r.kind,
    threshold: r.threshold ?? 0,
    severity: r.severity,
  };
  ruleOpen.value = true;
}

async function removeRule(r: AlertRule) {
  if (!(await confirmDialog({ message: t("al.deleteConfirm", { name: r.name }), danger: true })))
    return;
  try {
    await alerts.removeRule(r.id);
    await rules.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

const evCols = [
  { key: "ts", label: t("al.col.ts") },
  { key: "severity", label: t("al.col.severity") },
  { key: "ruleName", label: t("al.col.rule") },
  { key: "job", label: t("al.col.job") },
  { key: "age", label: t("al.col.age"), align: "right" as const, sortable: false },
  { key: "message", label: t("al.col.message"), sortable: false },
  { key: "op", label: "", align: "right" as const, sortable: false },
];
const age = (ts: string) => {
  const s = (Date.now() - new Date(ts).getTime()) / 1000;
  if (!Number.isFinite(s) || s < 0) return "—";
  return s < 60
    ? `${Math.max(1, Math.round(s))}s`
    : s < 3600
      ? `${Math.round(s / 60)}m`
      : s < 86400
        ? `${Math.round(s / 3600)}h`
        : `${Math.round(s / 86400)}d`;
};
const ruleCols = computed(() => [
  { key: "name", label: t("al.col.ruleName") },
  { key: "kind", label: t("al.col.kind") },
  { key: "severity", label: t("al.col.severity") },
  { key: "threshold", label: t("al.col.threshold"), align: "right" as const },
  { key: "enabled", label: t("al.col.enabled") },
  { key: "op", label: "", align: "right" as const, sortable: false },
]);
</script>

<template>
  <PageHead
    :title="t('page.alerts')"
    :sub="t('al.sub', { open: openEvents.length, rules: rules.data?.value?.items.length ?? 0 })"
    :badge="openEvents.length ? t('al.badgeOpen', { n: openEvents.length }) : t('al.badgeNone')"
    :tone="openEvents.length ? 'danger' : 'ok'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))">
    <Kpi
      :label="t('al.kpi.p1')"
      :value="openEvents.filter((e) => e.severity === 'danger').length"
      :tone="openEvents.some((e) => e.severity === 'danger') ? 'danger' : 'fg'"
      :foot="t('al.p1Foot')"
    />
    <Kpi
      :label="t('al.kpi.p2')"
      :value="openEvents.filter((e) => e.severity === 'warn').length"
      tone="warn"
      :foot="t('al.p2Foot')"
    />
    <Kpi
      :label="t('al.kpi.p3')"
      :value="openEvents.filter((e) => e.severity === 'info').length"
      :foot="t('al.p3Foot')"
    />
    <Kpi :label="t('al.kpi.acked')" :value="acked.length" :foot="t('al.ackedFoot')" />
  </div>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'live', label: t('al.tab.live'), badge: openEvents.length },
      { key: 'rule', label: t('al.tab.rules'), badge: rules.data?.value?.items.length ?? 0 },
      { key: 'hist', label: t('al.tab.acked'), badge: acked.length },
    ]"
  />

  <DataTable
    v-if="tab === 'live'"
    :columns="evCols"
    :rows="openEvents"
    row-key="id"
    :page-size="20"
    :empty="t('al.emptyOpen')"
  >
    <template #cell-ts="{ row }"
      ><span class="font-mono text-3xs text-fg-2">{{ fmtLocal(row.ts) }}</span></template
    >
    <template #cell-severity="{ row }"
      ><Badge
        :tone="row.severity === 'danger' ? 'danger' : row.severity === 'warn' ? 'warn' : 'idle'"
        >{{ row.severity }}</Badge
      ></template
    >
    <template #cell-job="{ row }">
      <RouterLink
        :to="`/jobs/${row.cluster}/${row.namespace}/${row.job}`"
        class="font-mono text-2xs hover:text-primary"
        >{{ row.job }}</RouterLink
      >
    </template>
    <template #cell-message="{ row }"
      ><span class="text-2xs text-fg-1">{{ row.message }}</span></template
    >
    <template #cell-age="{ row }"
      ><span class="font-mono text-3xs text-fg-2">{{ age(String(row.ts)) }}</span></template
    >
    <template #cell-op="{ row }">
      <button class="text-2xs text-primary hover:underline" @click="ack(String(row.id))">
        {{ t("al.ack") }}
      </button>
    </template>
  </DataTable>

  <DataTable
    v-else-if="tab === 'rule'"
    :columns="ruleCols"
    :rows="rules.data?.value?.items ?? []"
    row-key="id"
    :empty="t('al.emptyRules')"
  >
    <template #cell-kind="{ row }"
      ><span class="font-mono text-2xs">{{ row.kind }}</span></template
    >
    <template #cell-severity="{ row }"
      ><Badge :tone="row.severity === 'danger' ? 'danger' : 'warn'">{{
        row.severity
      }}</Badge></template
    >
    <template #cell-threshold="{ row }">{{ row.threshold ?? "—" }}</template>
    <template #cell-enabled="{ row }"
      ><Badge :tone="row.enabled ? 'ok' : 'idle'">{{
        row.enabled ? t("al.enabled") : t("al.disabled")
      }}</Badge></template
    >
    <template #cell-op="{ row }">
      <div class="flex justify-end gap-2">
        <button
          class="text-2xs text-primary hover:underline"
          @click="editRule(row as unknown as AlertRule)"
        >
          {{ t("al.edit") }}
        </button>
        <button
          class="text-2xs text-danger hover:underline"
          @click="removeRule(row as unknown as AlertRule)"
        >
          {{ t("al.delete") }}
        </button>
      </div>
    </template>
  </DataTable>

  <DataTable
    v-else
    :columns="evCols"
    :rows="acked"
    row-key="id"
    :page-size="20"
    :empty="t('al.emptyAcked')"
  />

  <Modal
    v-model="ruleOpen"
    :title="form.id ? t('al.editRuleTitle', { name: form.name }) : t('al.newRuleTitle')"
    width="580px"
  >
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("al.ruleName") }}</span>
        <input
          v-model="form.name"
          :placeholder="t('al.ruleNamePlaceholder')"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("al.type") }}</span>
        <select v-model="form.kind" class="rounded-lg border border-line bg-surface-2 px-2 py-1.5">
          <option v-for="k in kinds" :key="k.key" :value="k.key">
            {{ k.label }} — {{ k.hint }}
          </option>
        </select></label
      >
      <label v-if="form.kind === 'restarts_high'" class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("al.threshold") }}</span>
        <input
          v-model.number="form.threshold"
          type="number"
          min="0"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("al.severity") }}</span>
        <select
          v-model="form.severity"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
        >
          <option value="warn">warn</option>
          <option value="danger">danger</option>
          <option value="info">info</option>
        </select></label
      >
      <p class="text-2xs text-fg-2">{{ t("al.ruleNote") }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="ruleOpen = false"
      >
        {{ t("al.cancel") }}
      </button>
      <button
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
        @click="saveRule"
      >
        {{ t("al.save") }}
      </button>
    </template>
  </Modal>
</template>
