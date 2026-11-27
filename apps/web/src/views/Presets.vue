<script setup lang="ts">
import { computed, ref } from "vue";
import { useQuery, useQueryClient } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import { jobs, profiles } from "../api/client";
import { notify } from "../lib/notify";
import type { Finding, PresetItem } from "../api/types";
import { t } from "../lib/i18n";

const qc = useQueryClient();
const picked = ref("");
const edit = ref(false);
const busy = ref(false);
const draft = ref<PresetItem | null>(null);
const kvs = ref<{ k: string; v: string }[]>([]);
const labs = ref<{ k: string; v: string }[]>([]);

const q = useQuery({ queryKey: ["profiles"], queryFn: profiles.list });
const items = computed(() => (q.data?.value?.items ?? []) as PresetItem[]);
const cur = computed(() => items.value.find((p) => p.id === picked.value) ?? items.value[0]);

const defaultsView = computed(
  () => Object.entries(cur.value?.defaults ?? {}) as [string, string][],
);

// 与现网的偏差：基线是"应该长什么样"，作业是"实际长什么样"
const allJobs = useQuery({
  queryKey: ["jobs"],
  queryFn: () => jobs.list(),
  refetchInterval: 60_000,
});
const drift = computed(() => {
  const list = allJobs.data?.value?.items ?? [];
  const c = cur.value;
  if (!c || !list.length) return null;
  // 展示层统一成 1.15（与列表一致）；编辑层允许填 v1_15。
  const wantVer = String(c.flinkVersion ?? "")
    .replace(/^v/, "")
    .replace("_", ".");
  const verBad = list.filter((j) => String(j.flinkVersion ?? "") !== wantVer);
  const restBad = list.filter((j) => String(j.restExposed ?? "") !== String(c.restExposed ?? ""));
  const nsBad = list.filter((j) => j.namespace !== c.namespace);
  return {
    total: list.length,
    ver: {
      n: verBad.length,
      want: wantVer,
      sample: [...new Set(verBad.map((j) => j.flinkVersion))].slice(0, 3),
    },
    rest: {
      n: restBad.length,
      want: c.restExposed,
      sample: [...new Set(restBad.map((j) => j.restExposed))].slice(0, 3),
    },
    ns: {
      n: nsBad.length,
      want: c.namespace,
      sample: [...new Set(nsBad.map((j) => j.namespace))].slice(0, 3),
    },
  };
});
const labelsView = computed(() => Object.entries(cur.value?.labels ?? {}) as [string, string][]);

function startEdit() {
  if (!cur.value) return;
  const copy: PresetItem = JSON.parse(JSON.stringify(cur.value));
  draft.value = copy;
  kvs.value = Object.entries((copy.defaults ?? {}) as Record<string, unknown>).map(([k, v]) => ({
    k,
    v: String(v),
  }));
  labs.value = Object.entries((copy.labels ?? {}) as Record<string, unknown>).map(([k, v]) => ({
    k,
    v: String(v),
  }));
  edit.value = true;
}

function startNew() {
  draft.value = {
    id: "",
    name: "",
    builtin: false,
    flinkVersion: "v1_15",
    image: "",
    namespace: "flink",
    serviceAccount: "flink",
    restExposed: "NodePort",
    metricsPort: 9250,
    checkpointPrefix: "",
    savepointPrefix: "",
    defaults: {},
    labels: {},
  };
  kvs.value = [];
  labs.value = [{ k: "flinkops.io/scrape", v: "true" }];
  edit.value = true;
  picked.value = "";
}

function cancel() {
  edit.value = false;
  draft.value = null;
}

async function save() {
  const d = draft.value;
  if (!d?.id?.trim()) {
    notify(t("preset.idRequired"), "warn");
    return;
  }
  d.defaults = Object.fromEntries(
    kvs.value.filter((x) => x.k.trim()).map((x) => [x.k.trim(), x.v]),
  );
  d.labels = Object.fromEntries(labs.value.filter((x) => x.k.trim()).map((x) => [x.k.trim(), x.v]));
  d.metricsPort = Number(d.metricsPort) || 9250;
  // 版本两种写法都能存：展示层是 1.15，operator 要 v1_15
  const ver = String(d.flinkVersion ?? "").trim();
  if (/^\d+\.\d+$/.test(ver)) d.flinkVersion = `v${ver.replace(".", "_")}`;
  busy.value = true;
  try {
    const r = await profiles.save(d.id.trim(), d);
    const errors = ((r as { findings?: Finding[] }).findings ?? []).filter(
      (f) => f.severity === "error",
    );
    notify(
      errors.length
        ? t("preset.savedWithErrors", { n: errors.length, first: errors[0]?.message ?? "" })
        : t("preset.saved"),
      errors.length ? "warn" : "ok",
    );
    edit.value = false;
    picked.value = d.id.trim();
    await qc.invalidateQueries({ queryKey: ["profiles"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
  }
}

const pageActions = computed(() =>
  edit.value
    ? [
        { label: t("preset.cancel"), run: cancel },
        {
          label: busy.value ? t("preset.saving") : t("preset.saveBaseline"),
          run: save,
          primary: true,
        },
      ]
    : [
        { label: t("preset.editCurrent"), run: startEdit },
        { label: t("preset.newBaseline"), run: startNew, primary: true },
      ],
);
</script>

<template>
  <PageHead
    :title="t('page.presets')"
    :sub="edit ? t('preset.editingHint') : t('preset.sub', { n: items.length })"
    :badge="edit ? t('preset.editBadge') : 'Profile'"
    :tone="edit ? 'warn' : 'info'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="grid gap-3" style="grid-template-columns: 260px minmax(0, 1fr)">
    <Card :title="t('preset.list')" :pad="false">
      <button
        v-for="p in items"
        :key="p.id"
        class="flex w-full items-center gap-2 border-b border-line-2/50 px-3 py-2 text-left text-xs last:border-0 hover:bg-hover/40"
        :class="cur?.id === p.id && !edit ? 'bg-surface-2 text-fg' : 'text-fg-1'"
        @click="
          picked = p.id;
          edit = false;
        "
      >
        <span class="truncate font-mono">{{ p.id }}</span>
        <Badge v-if="p.builtin" tone="idle">{{ t("preset.builtin") }}</Badge>
        <span class="ml-auto font-mono text-3xs text-fg-2">{{ p.flinkVersion }}</span>
      </button>
      <div v-if="!items.length" class="px-3 py-6 text-center text-xs text-fg-2">
        {{ t("preset.empty") }}
      </div>
    </Card>

    <!-- 编辑态 -->
    <Card
      v-if="edit && draft"
      :title="draft.id ? t('preset.editTitle', { id: draft.id }) : t('preset.newBaseline')"
      :hint="t('preset.builtinHint')"
    >
      <div class="grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(220px, 1fr))">
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.id") }}</span>
          <input
            v-model="draft.id"
            :disabled="!!cur && draft.id === cur.id && !draft.builtin"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono disabled:opacity-60"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.name") }}</span>
          <input v-model="draft.name" class="rounded-lg border border-line bg-surface-2 px-2 py-1"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.image") }}</span>
          <input
            v-model="draft.image"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.flinkVersion") }}</span>
          <input
            v-model="draft.flinkVersion"
            placeholder="v1_15"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.namespace") }}</span>
          <input
            v-model="draft.namespace"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">ServiceAccount</span>
          <input
            v-model="draft.serviceAccount"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.restExposed") }}</span>
          <select
            v-model="draft.restExposed"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          >
            <option value="ClusterIP">ClusterIP</option>
            <option value="NodePort">NodePort</option>
            <option value="LoadBalancer">LoadBalancer</option>
          </select></label
        >
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.metricsPort") }}</span>
          <input
            v-model.number="draft.metricsPort"
            type="number"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.ckptPrefix") }}</span>
          <input
            v-model="draft.checkpointPrefix"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("preset.spPrefix") }}</span>
          <input
            v-model="draft.savepointPrefix"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
        /></label>
      </div>

      <div class="mt-4 border-t border-line-2/60 pt-3">
        <div class="mb-1.5 flex items-center gap-2 text-2xs text-fg-2">
          <span>{{ t("preset.cfgDefaultsCount", { n: kvs.length }) }}</span>
          <button
            class="rounded border border-line px-2 py-0.5 text-2xs hover:bg-hover"
            @click="kvs.push({ k: '', v: '' })"
          >
            {{ t("preset.addRow") }}
          </button>
        </div>
        <div class="flex flex-col gap-1">
          <div v-for="(row, i) in kvs" :key="i" class="flex gap-2">
            <input
              v-model="row.k"
              placeholder="k"
              class="w-[52%] rounded border border-line bg-surface-2 px-2 py-0.5 font-mono text-2xs"
            />
            <input
              v-model="row.v"
              placeholder="v"
              class="flex-1 rounded border border-line bg-surface-2 px-2 py-0.5 font-mono text-2xs"
            />
            <button class="px-1 text-2xs text-danger hover:underline" @click="kvs.splice(i, 1)">
              {{ t("preset.removeRow") }}
            </button>
          </div>
        </div>
      </div>

      <div class="mt-4 border-t border-line-2/60 pt-3">
        <div class="mb-1.5 flex items-center gap-2 text-2xs text-fg-2">
          <span>{{ t("preset.labelsCount", { n: labs.length }) }}</span>
          <button
            class="rounded border border-line px-2 py-0.5 text-2xs hover:bg-hover"
            @click="labs.push({ k: '', v: '' })"
          >
            {{ t("preset.addRow") }}
          </button>
        </div>
        <div class="flex flex-col gap-1">
          <div v-for="(row, i) in labs" :key="i" class="flex gap-2">
            <input
              v-model="row.k"
              placeholder="key"
              class="w-[52%] rounded border border-line bg-surface-2 px-2 py-0.5 font-mono text-2xs"
            />
            <input
              v-model="row.v"
              placeholder="value"
              class="flex-1 rounded border border-line bg-surface-2 px-2 py-0.5 font-mono text-2xs"
            />
            <button class="px-1 text-2xs text-danger hover:underline" @click="labs.splice(i, 1)">
              {{ t("preset.removeRow") }}
            </button>
          </div>
        </div>
      </div>
    </Card>

    <!-- 只读态 -->
    <Card
      v-else-if="cur"
      :title="cur.id"
      :hint="cur.builtin ? t('preset.builtinEditHint') : t('preset.customHint')"
    >
      <div class="grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(200px, 1fr))">
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">{{ t("preset.image") }}</span><span
            class="truncate font-mono text-2xs"
            >{{ cur.image }}</span
          >
        </div>
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">Flink</span
          ><span class="font-mono text-2xs">{{ cur.flinkVersion }}</span>
        </div>
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">{{ t("preset.namespace") }}</span><span
            class="font-mono text-2xs"
            >{{ cur.namespace }}</span
          >
        </div>
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">ServiceAccount</span
          ><span class="font-mono text-2xs">{{ cur.serviceAccount }}</span>
        </div>
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">{{ t("preset.restExposed") }}</span><span
            class="font-mono text-2xs"
            :class="cur.restExposed === 'NodePort' ? 'text-warn' : ''"
            >{{ cur.restExposed }}</span
          >
        </div>
        <div class="flex justify-between text-xs">
          <span class="text-fg-2">{{ t("preset.metricsPort") }}</span><span
            class="font-mono text-2xs"
            >{{ cur.metricsPort }}</span
          >
        </div>
      </div>

      <div class="mt-3 border-t border-line-2/60 pt-3">
        <div class="mb-1.5 text-2xs text-fg-2">{{ t("preset.pathTemplate") }}</div>
        <div class="flex flex-col gap-1 font-mono text-2xs text-fg-1">
          <div>checkpoints → {{ cur.checkpointPrefix }}</div>
          <div>savepoints → {{ cur.savepointPrefix }}</div>
        </div>
      </div>

      <div class="mt-3 border-t border-line-2/60 pt-3">
        <div class="mb-1.5 text-2xs text-fg-2">
          {{ t("preset.cfgDefaultsCount", { n: defaultsView.length }) }}
        </div>
        <div class="max-h-[260px] overflow-auto">
          <div
            v-for="[k, v] in defaultsView"
            :key="k"
            class="flex gap-3 border-b border-line-2/40 py-1 text-2xs last:border-0"
          >
            <span class="w-[55%] truncate font-mono text-fg-1" :title="k">{{ k }}</span>
            <span class="truncate font-mono text-fg-2">{{ v }}</span>
          </div>
        </div>
      </div>

      <div class="mt-3 border-t border-line-2/60 pt-3">
        <div class="mb-1.5 text-2xs text-fg-2">{{ t("preset.labelsPodMonitor") }}</div>
        <div class="flex flex-wrap gap-2">
          <Badge v-for="[k, v] in labelsView" :key="k" tone="idle">{{ k }}={{ v }}</Badge>
        </div>
      </div>
    </Card>
  </div>

  <Card
    v-if="drift && !edit"
    :title="t('preset.driftCard')"
    :hint="t('preset.driftTotal', { n: drift.total, id: cur?.id ?? '' })"
    class="mt-3"
  >
    <div class="grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(220px, 1fr))">
      <div class="rounded-lg border border-line-2 px-3 py-2">
        <div class="flex items-center justify-between text-2xs">
          <span class="text-fg-2">{{ t("preset.flinkVersion") }}</span>
          <span class="font-mono" :class="drift.ver.n ? 'text-warn' : 'text-ok'">{{
            drift.ver.n ? t("preset.mismatch", { n: Number(drift.ver.n) }) : t("preset.allMatch")
          }}</span>
        </div>
        <div class="mt-1 text-3xs text-fg-2">
          {{
            t("preset.driftLine", {
              want: String(drift.ver.want ?? ""),
              sample: drift.ver.sample.join(" / ") || "—",
            })
          }}
        </div>
      </div>
      <div class="rounded-lg border border-line-2 px-3 py-2">
        <div class="flex items-center justify-between text-2xs">
          <span class="text-fg-2">{{ t("preset.restExposed") }}</span>
          <span class="font-mono" :class="drift.rest.n ? 'text-warn' : 'text-ok'">{{
            drift.rest.n ? t("preset.mismatch", { n: Number(drift.rest.n) }) : t("preset.allMatch")
          }}</span>
        </div>
        <div class="mt-1 text-3xs text-fg-2">
          {{
            t("preset.driftLine", {
              want: String(drift.rest.want ?? ""),
              sample: drift.rest.sample.join(" / ") || "—",
            })
          }}
        </div>
      </div>
      <div class="rounded-lg border border-line-2 px-3 py-2">
        <div class="flex items-center justify-between text-2xs">
          <span class="text-fg-2">{{ t("preset.nsShort") }}</span>
          <span class="font-mono" :class="drift.ns.n ? 'text-warn' : 'text-ok'">{{
            drift.ns.n ? t("preset.notInNs", { n: Number(drift.ns.n) }) : t("preset.allMatch")
          }}</span>
        </div>
        <div class="mt-1 text-3xs text-fg-2">
          {{
            t("preset.driftLine", {
              want: String(drift.ns.want ?? ""),
              sample: drift.ns.sample.join(" / ") || "—",
            })
          }}
        </div>
      </div>
    </div>
    <p class="mt-2 text-2xs leading-relaxed text-fg-2">{{ t("preset.driftNote") }}</p>
  </Card>
</template>
