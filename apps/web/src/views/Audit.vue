<script setup lang="ts">
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import Modal from "../components/Modal.vue";
import Donut from "../components/charts/Donut.vue";
import BarMini from "../components/charts/BarMini.vue";
import { audit, type AuditEntry } from "../api/client";
import { notify } from "../lib/notify";
import { fmtLocal, tzLabel, utcOf } from "../lib/time";
import { t } from "../lib/i18n";

const kw = ref("");
const days = ref(0);
const result = ref("");
const actor = ref("");
const action = ref("");
const page = ref(1);
const picked = ref<AuditEntry | null>(null);
const PAGE_SIZE = 20;

// 审计量大时前端只过滤"最近一页"会漏：直接让服务端按条件查（上限 2000/页）
const serverFilter = computed(() => ({
  actor: actor.value || undefined,
  action: action.value || undefined,
  result: result.value || undefined,
  q: kw.value.trim() || undefined,
  limit: 2000,
}));
const q = useQuery({
  queryKey: ["audit", serverFilter],
  queryFn: () => audit.list(serverFilter.value),
  refetchInterval: 20_000,
});

function exportCsv() {
  window.open(audit.csvUrl(serverFilter.value), "_blank");
  notify(t("audit.csvStarted"), "ok");
}

const ACTION_LABEL: Record<string, string> = {
  "auth.login": t("audit.act.login"),
  "auth.logout": t("audit.act.logout"),
  "jobs.sync": t("audit.act.jobsSync"),
  "job.start": t("audit.act.jobStart"),
  "job.stop": t("audit.act.jobStop"),
  "job.restart": t("audit.act.jobRestart"),
  "job.delete": t("audit.act.jobDelete"),
  "job.apply": t("audit.act.apply"),
  "savepoint.trigger": t("audit.act.savepoint"),
  "job.savepoint": t("audit.act.savepoint2"),
  "ui.open": t("audit.act.openUi"),
  "proxy.blocked": t("audit.act.proxyBlocked"),
  "alert.ack": t("audit.act.alertAck"),
  "profile.save": t("audit.act.profileSave"),
};
const label = (a: string) => ACTION_LABEL[a] ?? a;

const all = computed(() => q.data?.value?.items ?? []);

const filtered = computed(() =>
  all.value
    .filter((e) =>
      !days.value ? true : new Date(e.ts).getTime() >= Date.now() - days.value * 86400_000,
    )
    .filter((e) => (result.value ? e.result === result.value : true))
    .filter((e) =>
      kw.value
        ? `${e.actor} ${e.action} ${e.target} ${e.cluster} ${e.namespace}`
            .toLowerCase()
            .includes(kw.value.toLowerCase())
        : true,
    ),
);

const pages = computed(() => Math.max(1, Math.ceil(filtered.value.length / PAGE_SIZE)));
const rows = computed(() =>
  filtered.value.slice((page.value - 1) * PAGE_SIZE, page.value * PAGE_SIZE),
);

const abnormal = computed(() => all.value.filter((e) => e.result !== "ok").slice(0, 6));

const byAction = computed(() => {
  const m = new Map<string, number>();
  for (const e of filtered.value) m.set(label(e.action), (m.get(label(e.action)) ?? 0) + 1);
  return [...m.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 6)
    .map(([l, v]) => ({ label: l, value: v }));
});

const hourly = computed(() => {
  const buckets = new Array(24).fill(0);
  const start = new Date();
  start.setMinutes(0, 0, 0);
  for (const e of filtered.value) {
    const diff = Math.floor((start.getTime() - new Date(e.ts).getTime()) / 3600_000);
    if (diff >= 0 && diff < 24) buckets[23 - diff] += 1;
  }
  return buckets;
});

const ts = (v: string) => fmtLocal(v);
const copyRequestId = (id: string) => {
  void navigator.clipboard?.writeText(id);
  notify(t("audit.copied"), "ok");
};

const pageActions = computed(() => [
  {
    label: t("audit.export"),
    run: () => {
      // CSV 必须转义：detail 里是 JSON（含逗号/引号/换行），不转义会把列打散
      const esc = (v: unknown) => {
        const s = v == null ? "" : String(v);
        return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
      };
      const head = [
        t("audit.csv.time"),
        t("audit.csv.actor"),
        t("audit.csv.action"),
        t("audit.csv.target"),
        t("audit.csv.cluster"),
        t("audit.csv.ns"),
        t("audit.csv.result"),
        t("audit.csv.detail"),
      ];
      const body = [
        head.join(","),
        ...filtered.value.map((e) =>
          [
            ts(e.ts),
            e.actor,
            e.action,
            e.target,
            e.cluster,
            e.namespace,
            e.result,
            JSON.stringify(e.detail ?? ""),
          ]
            .map(esc)
            .join(","),
        ),
      ];
      // BOM 让 Excel 直接按 UTF-8 打开中文
      const blob = new Blob(["\uFEFF" + body.join("\n")], { type: "text/csv;charset=utf-8" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `flinkops-audit-${Date.now()}.csv`;
      a.click();
      notify(t("audit.exported", { n: filtered.value.length }), "ok");
    },
  },
  {
    label: t("audit.reset"),
    run: () => {
      kw.value = "";
      result.value = "";
      days.value = 0;
      page.value = 1;
    },
  },
]);
</script>

<template>
  <PageHead :title="t('page.audit')" :sub="t('audit.sub', { n: filtered.length, tz: tzLabel() })">
    <template #actions
      ><PageActions :items="[{ label: t('audit.exportCsv'), run: exportCsv }, ...pageActions]"
    /></template>
  </PageHead>

  <!-- 筛选 -->
  <div class="mb-3 rounded-xl border border-line-2 bg-surface px-4 py-2.5">
    <div class="flex flex-wrap items-center gap-2 text-xs">
      <div class="inline-flex overflow-hidden rounded-lg border border-line text-2xs">
        <button
          v-for="d in [
            { k: 0, t: t('audit.range.all') },
            { k: 1, t: t('audit.range.today') },
            { k: 7, t: t('audit.range.7d') },
            { k: 30, t: t('audit.range.30d') },
          ]"
          :key="d.k"
          class="px-2.5 py-1"
          :class="days === d.k ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'"
          @click="
            days = d.k;
            page = 1;
          "
        >
          {{ d.t }}
        </button>
      </div>
      <select
        v-model="result"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1"
        @change="page = 1"
      >
        <option value="">{{ t("audit.allResults") }}</option>
        <option value="ok">ok</option>
        <option value="denied">denied</option>
        <option value="error">error</option>
      </select>
      <input
        v-model="kw"
        :placeholder="t('audit.search')"
        class="w-64 rounded-lg border border-line bg-surface-2 px-2 py-1"
        @input="page = 1"
      />
      <span class="ml-auto text-2xs text-fg-2">{{
        t("audit.countLine2", { n: filtered.length, size: PAGE_SIZE })
      }}</span>
    </div>
  </div>

  <div class="mb-3 grid gap-3 md:grid-cols-2">
    <Card :title="t('audit.distCard')" :hint="t('audit.distHint')">
      <div class="flex items-center gap-4">
        <Donut
          :items="byAction"
          :size="118"
          :center-label="t('audit.donutCenter')"
          :center-value="String(filtered.length)"
        />
        <div class="min-w-0 flex-1">
          <div class="mb-1 text-2xs text-fg-2">{{ t("audit.dist24h") }}</div>
          <BarMini :data="hourly" :height="46" />
          <div class="mt-1 flex justify-between font-mono text-3xs text-fg-2">
            <span>{{ t("audit.ago24h") }}</span><span>{{ t("audit.now") }}</span>
          </div>
        </div>
      </div>
    </Card>

    <Card :title="t('audit.abnormalCard')" hint="result != ok" :pad="false">
      <div class="divide-y divide-line-2/70">
        <button
          v-for="e in abnormal"
          :key="e.id"
          class="flex w-full items-center gap-2 px-4 py-2 text-left hover:bg-hover"
          @click="picked = e"
        >
          <Badge :tone="e.result === 'denied' ? 'warn' : 'danger'">{{ e.result }}</Badge>
          <div class="min-w-0 flex-1">
            <div class="truncate text-xs">{{ label(e.action) }} · {{ e.target || "—" }}</div>
            <div class="truncate text-3xs text-fg-2">{{ e.detail || "—" }}</div>
          </div>
          <span class="shrink-0 font-mono text-3xs text-fg-2">{{ ts(e.ts).slice(11) }}</span>
        </button>
        <div v-if="!abnormal.length" class="px-4 py-6 text-center text-xs text-fg-2">
          {{ t("audit.abnormalEmpty") }}
        </div>
      </div>
    </Card>
  </div>

  <Card :title="t('audit.detailCard')" :hint="`第 ${page} / ${pages} 页`" :pad="false">
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("audit.col.time") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.actor") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.action") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.target") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.cluster") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.result") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.ip") }}</th>
            <th class="px-3 py-2">{{ t("audit.col.requestId") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="e in rows"
            :key="e.id"
            class="cursor-pointer border-b border-line-2/60 last:border-0 hover:bg-hover"
            @click="picked = e"
          >
            <td
              class="whitespace-nowrap px-3 py-1.5 font-mono text-3xs text-fg-2"
              :title="utcOf(e.ts)"
            >
              {{ ts(e.ts) }}
            </td>
            <td class="px-3 py-1.5">{{ e.actor }}</td>
            <td class="px-3 py-1.5">
              <span class="font-mono text-2xs">{{ label(e.action) }}</span>
            </td>
            <td class="px-3 py-1.5">
              <div class="font-mono text-2xs">{{ e.target || "—" }}</div>
              <div class="font-mono text-3xs text-fg-2">{{ e.namespace || "—" }}</div>
            </td>
            <td class="px-3 py-1.5 font-mono text-2xs">{{ e.cluster || "—" }}</td>
            <td class="px-3 py-1.5">
              <Badge :tone="e.result === 'ok' ? 'ok' : e.result === 'denied' ? 'warn' : 'danger'">{{
                e.result
              }}</Badge>
            </td>
            <td class="px-3 py-1.5 font-mono text-3xs text-fg-2">
              {{ (e as unknown as { ip?: string }).ip ?? "—" }}
            </td>
            <td class="px-3 py-1.5 font-mono text-3xs text-fg-2" :title="e.id">
              {{ e.id.slice(0, 8) }}
            </td>
          </tr>
          <tr v-if="!rows.length">
            <td colspan="8" class="px-3 py-8 text-center text-xs text-fg-2">
              {{ t("audit.empty") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <div class="flex items-center justify-between border-t border-line-2 px-3 py-2 text-2xs">
      <span class="text-fg-2">{{ t("audit.note") }}</span>
      <div class="flex items-center gap-1">
        <button
          class="rounded border border-line px-2 py-0.5 disabled:opacity-40"
          :disabled="page <= 1"
          @click="page -= 1"
        >
          {{ t("audit.prev") }}
        </button>
        <span class="px-1 font-mono">{{ page }} / {{ pages }}</span>
        <button
          class="rounded border border-line px-2 py-0.5 disabled:opacity-40"
          :disabled="page >= pages"
          @click="page += 1"
        >
          {{ t("audit.next") }}
        </button>
      </div>
    </div>
  </Card>

  <Modal
    :model-value="!!picked"
    :title="t('audit.detail.title')"
    :sub="picked ? `${picked.action} · ${ts(picked.ts)}` : ''"
    width="620px"
    @update:model-value="picked = null"
  >
    <div v-if="picked" class="flex flex-col gap-1.5 text-xs">
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.detail.actor") }}</span><span class="font-mono">{{
          picked.actor
        }}</span>
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.col.action") }}</span><span class="font-mono"
          >{{ picked.action }}（{{ label(picked.action) }}）</span
        >
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.detail.target") }}</span><span class="font-mono">{{
          picked.target || "—"
        }}</span>
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.detail.clusterNs") }}</span><span class="font-mono"
          >{{ picked.cluster || "—" }} / {{ picked.namespace || "—" }}</span
        >
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.col.result") }}</span><Badge
          :tone="picked.result === 'ok' ? 'ok' : picked.result === 'denied' ? 'warn' : 'danger'"
          >{{ picked.result }}</Badge
        >
      </div>
      <div class="flex justify-between">
        <span class="text-fg-2">{{ t("audit.col.requestId") }}</span>
        <span class="inline-flex items-center gap-2 font-mono text-2xs">
          {{ picked.id }}
          <button
            class="rounded border border-line px-1.5 py-0.5 text-3xs"
            @click="copyRequestId(picked.id)"
          >
            {{ t("audit.detail.copy") }}
          </button>
        </span>
      </div>
      <div class="mt-1 border-t border-line-2 pt-2">
        <div class="mb-1 text-fg-2">{{ t("audit.detail.meta") }}</div>
        <pre class="max-h-[240px] overflow-auto rounded-lg bg-surface-2 p-2 font-mono text-2xs">{{
          picked.detail ?? t("audit.none")
        }}</pre>
      </div>
    </div>
  </Modal>
</template>
