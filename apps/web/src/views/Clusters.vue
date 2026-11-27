<script setup lang="ts">
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { useRouter } from "vue-router";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Badge from "../components/Badge.vue";
import Modal from "../components/Modal.vue";
import { auth, clusterAdmin, jobs, prom, system } from "../api/client";
import { notify } from "../lib/notify";
import { t } from "../lib/i18n";

const router = useRouter();
const me = useQuery({ queryKey: ["me"], queryFn: auth.me });
const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });
const caps = useQuery({ queryKey: ["cluster-info"], queryFn: system.clusterInfo, retry: false });

const items = computed(() => all.data?.value?.items ?? []);
const capsById = computed(() => new Map((caps.data?.value?.clusters ?? []).map((c) => [c.id, c])));

const nodes = useQuery({
  queryKey: ["clusters", "nodes"],
  queryFn: () => prom.query("count(kube_node_info)"),
  retry: false,
});
const nodeCount = computed(() => Number(nodes.data?.value?.data?.result?.[0]?.value?.[1] ?? NaN));

const rows = computed(() =>
  (me.data?.value?.clusters ?? []).map((id) => {
    const mine = items.value.filter((j) => j.cluster === id);
    const cap = capsById.value.get(id);
    const op = cap?.operators?.[0];
    return {
      id,
      inCluster: cap?.inCluster ?? false,
      jobs: mine.length,
      running: mine.filter((j) => j.state === "RUNNING").length,
      red: mine.filter((j) => j.health === "red").length,
      ns: new Set(mine.map((j) => j.namespace)).size,
      nodeport: mine.filter((j) => j.restExposed === "NodePort").length,
      kinds: cap?.supportedKinds ?? [],
      opVersion: op?.images?.[0]?.split(":").pop() ?? "—",
      opReady: op?.readyReplicas ?? 0,
      opNs: op?.namespace ?? "—",
    };
  }),
);

const opTotal = computed(() => rows.value.reduce((n, r) => n + (r.opReady > 0 ? 1 : 0), 0));
const nodeportCount = computed(() => rows.value.reduce((n, r) => n + r.nodeport, 0));
const failedCount = computed(() => rows.value.reduce((n, r) => n + r.red, 0));

// 接入
const addOpen = ref(false);
const addId = ref("");
const addPath = ref("");
/** 接入方式：路径（服务端可读）| 粘贴（直接贴 kubeconfig 文本） */
const addMode = ref<"path" | "paste">("path");
const addText = ref("");
const testing = ref(false);
const testResult = ref<
  | { ok: true; version: string; jobCount: number; kinds: number; latencyMs: number }
  | { ok: false; error: string }
  | null
>(null);

function addBody() {
  const paste = addMode.value === "paste" ? addText.value.trim() : "";
  return paste ? { kubeconfig: paste } : { kubeconfigPath: addPath.value.trim() || undefined };
}

/** 测试连通性：只探活，不写入 */
async function doTest() {
  if (addMode.value === "paste" && !addText.value.trim()) {
    notify(t("clusters.add.needPaste"), "warn");
    return;
  }
  testing.value = true;
  testResult.value = null;
  try {
    const r = await clusterAdmin.test(addBody());
    testResult.value = {
      ok: true,
      version: r.serverVersion,
      jobCount: r.jobCount,
      kinds: r.supportedKinds.length,
      latencyMs: r.latencyMs,
    };
  } catch (e) {
    testResult.value = { ok: false, error: (e as Error).message };
  } finally {
    testing.value = false;
  }
}
async function doAdd() {
  if (!addId.value.trim()) {
    notify(t("clusters.add.needId"), "warn");
    return;
  }
  try {
    const r = await clusterAdmin.add({ id: addId.value.trim(), ...addBody() });
    notify(
      t("clusters.add.done", {
        cluster: r.cluster,
        jobs: r.jobCount,
        kinds: r.supportedKinds.length,
      }),
      "ok",
      6000,
    );
    addOpen.value = false;
    addId.value = "";
    addPath.value = "";
    addText.value = "";
    testResult.value = null;
    await me.refetch();
    await all.refetch();
    await caps.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function reconnect(id: string) {
  try {
    const r = await clusterAdmin.reconnect(id);
    notify(t("clusters.reconnected", { cluster: r.cluster, jobs: r.jobCount }), "ok");
    await caps.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

const INSTALL = `helm repo add flink-operator https://downloads.apache.org/flink-kubernetes-operator-1.10.0/
helm install flink-kubernetes-operator flink-operator/flink-kubernetes-operator \\
  -n flink-kubernetes-operator --create-namespace \\
  --set webhook.create=true`;

const pageActions = [
  { label: t("clusters.actions.add"), run: () => (addOpen.value = true) },
  {
    label: t("clusters.actions.refresh"),
    run: async () => {
      await me.refetch();
      await all.refetch();
      await caps.refetch();
      notify(t("clusters.refreshed"), "ok");
    },
  },
];
</script>

<template>
  <PageHead
    :title="t('page.clusters')"
    :sub="t('clusters.sub', { n: rows.length })"
    :badge="t('clusters.badge', { n: rows.length })"
    tone="ok"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="mb-3 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
    <Kpi
      :label="t('clusters.kpi.connected')"
      :value="rows.length"
      :foot="t('clusters.kpi.opFoot', { n: opTotal })"
    />
    <Kpi
      :label="t('clusters.kpi.jobs')"
      :value="items.length"
      :foot="t('clusters.kpi.jobFoot', { n: items.filter((j) => j.state === 'RUNNING').length })"
    />
    <Kpi
      :label="t('clusters.kpi.nodes')"
      :value="Number.isFinite(nodeCount) ? nodeCount : '—'"
      foot="kube_node_info"
    />
    <Kpi
      :label="t('clusters.kpi.operator')"
      :value="rows[0]?.opVersion ?? '—'"
      :foot="t('clusters.kpi.imageFoot')"
    />
  </div>

  <Card
    :title="t('clusters.card.capability')"
    :hint="t('clusters.card.capabilityHint')"
    :pad="false"
    class="mb-3"
  >
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("clusters.col.cluster") }}</th>
            <th class="px-3 py-2">Operator</th>
            <th class="px-3 py-2">CRD</th>
            <th class="px-3 py-2 text-right">{{ t("clusters.col.jobs") }}</th>
            <th class="px-3 py-2 text-right">{{ t("clusters.col.namespaces") }}</th>
            <th class="px-3 py-2 text-right">NodePort</th>
            <th class="px-3 py-2">{{ t("clusters.col.health") }}</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in rows" :key="r.id" class="border-b border-line-2/60 last:border-0">
            <td class="px-3 py-1.5">
              <div class="font-mono font-medium">{{ r.id }}</div>
              <div class="text-3xs text-fg-2">
                {{ r.inCluster ? t("clusters.inCluster") : t("clusters.outCluster") }}
              </div>
            </td>
            <td class="px-3 py-1.5">
              <div class="font-mono text-2xs">{{ r.opVersion }}</div>
              <div class="text-3xs text-fg-2">{{ r.opNs }} · ready {{ r.opReady }}</div>
            </td>
            <td class="px-3 py-1.5">
              <div class="flex flex-wrap gap-1">
                <span
                  v-for="k in r.kinds.slice(0, 4)"
                  :key="k"
                  class="rounded-full border border-line bg-surface-3 px-1 py-0.5 text-[9.5px] text-fg-2"
                  >{{ k }}</span
                >
              </div>
            </td>
            <td class="px-3 py-1.5 text-right font-mono">{{ r.jobs }}</td>
            <td class="px-3 py-1.5 text-right font-mono">{{ r.ns }}</td>
            <td
              class="px-3 py-1.5 text-right font-mono"
              :class="r.nodeport ? 'text-warn' : 'text-fg-2'"
            >
              {{ r.nodeport }}
            </td>
            <td class="px-3 py-1.5">
              <Badge :tone="r.red ? 'danger' : r.opReady ? 'ok' : 'warn'" dot>{{
                r.red
                  ? t("clusters.health.bad")
                  : r.opReady
                    ? t("clusters.health.ok")
                    : t("clusters.health.opNotReady")
              }}</Badge>
            </td>
            <td class="px-3 py-1.5 text-right">
              <button
                class="text-2xs text-primary hover:underline"
                @click="router.push(`/clusters/${r.id}`)"
              >
                {{ t("clusters.detail") }}
              </button>
              <button class="ml-2 text-2xs text-fg-2 hover:underline" @click="reconnect(r.id)">
                {{ t("clusters.reconnect") }}
              </button>
            </td>
          </tr>
          <tr v-if="!rows.length">
            <td colspan="8" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("clusters.none") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>

  <div class="grid gap-3 xl:grid-cols-[1.2fr_1fr]">
    <Card :title="t('clusters.install.title')" :hint="t('clusters.install.hint')">
      <pre
        class="overflow-auto rounded-lg bg-surface-2 p-3 font-mono text-3xs leading-relaxed text-fg-1"
        >{{ INSTALL }}</pre>
      <p class="mt-2 text-2xs leading-relaxed text-fg-2">
        {{ t("clusters.install.body") }}
      </p>
    </Card>

    <Card :title="t('clusters.hints.title')">
      <div class="flex flex-col gap-2 text-2xs">
        <div
          class="rounded-lg px-3 py-2"
          :class="rows.some((r) => r.nodeport) ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'"
        >
          {{ t("clusters.hints.nodeport", { n: nodeportCount }) }}
        </div>
        <div
          class="rounded-lg px-3 py-2"
          :class="rows.some((r) => r.red) ? 'bg-danger/10 text-danger' : 'bg-ok/10 text-ok'"
        >
          {{ t("clusters.hints.failed", { n: failedCount }) }}
        </div>
        <div class="rounded-lg px-3 py-2 bg-surface-2 text-fg-2">
          {{ t("clusters.hints.kinds")
          }}{{ (caps.data?.value?.clusters?.[0]?.supportedKinds ?? []).join(" / ") || "—" }}
        </div>
      </div>
    </Card>
  </div>

  <Modal
    v-model="addOpen"
    :title="t('clusters.add.title')"
    :sub="t('clusters.add.sub')"
    width="640px"
  >
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("clusters.add.id") }}</span>
        <input
          v-model="addId"
          placeholder="prod / staging"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      /></label>

      <div class="flex items-center gap-2">
        <span class="text-fg-2">{{ t("clusters.add.mode") }}</span>
        <div class="flex overflow-hidden rounded-lg border border-line">
          <button
            class="px-2.5 py-1 text-2xs transition"
            :class="
              addMode === 'path' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'
            "
            @click="addMode = 'path'"
          >
            {{ t("clusters.add.modePath") }}
          </button>
          <button
            class="px-2.5 py-1 text-2xs transition"
            :class="
              addMode === 'paste' ? 'bg-primary text-white' : 'bg-surface-2 text-fg-2 hover:text-fg'
            "
            @click="addMode = 'paste'"
          >
            {{ t("clusters.add.modePaste") }}
          </button>
        </div>
      </div>

      <label v-if="addMode === 'path'" class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("clusters.add.path") }}</span>
        <input
          v-model="addPath"
          :placeholder="t('clusters.add.pathPlaceholder')"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      /></label>

      <label v-else class="flex flex-col gap-1">
        <span class="text-fg-2">{{ t("clusters.add.pasteLabel") }}</span>
        <textarea
          v-model="addText"
          rows="10"
          spellcheck="false"
          placeholder="apiVersion: v1
kind: Config
clusters:
- name: prod
  cluster:
    server: https://10.0.0.1:6443
    certificate-authority-data: ...
users:
- name: admin
  user:
    client-certificate-data: ...
    client-key-data: ..."
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-3xs leading-relaxed"
        />
        <span class="text-2xs text-fg-2">{{ t("clusters.add.pastePrivacy") }}</span>
      </label>

      <p class="text-2xs text-fg-2">
        {{ t("clusters.add.testHint") }}
      </p>

      <div
        v-if="testResult"
        class="rounded-lg px-2.5 py-2 text-2xs leading-relaxed"
        :class="testResult.ok ? 'bg-ok/10 text-ok' : 'bg-danger/10 text-danger'"
      >
        <template v-if="testResult.ok">
          {{
            t("clusters.add.ok", {
              version: testResult.version,
              jobs: testResult.jobCount,
              kinds: testResult.kinds,
              ms: testResult.latencyMs,
            })
          }}
        </template>
        <template v-else>❌ {{ testResult.error }}</template>
      </div>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover disabled:opacity-50"
        :disabled="testing"
        @click="doTest"
      >
        {{ testing ? t("clusters.testing") : t("clusters.test") }}
      </button>
      <span class="flex-1"></span>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="addOpen = false"
      >
        {{ t("clusters.add.cancel") }}
      </button>
      <button class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white" @click="doAdd">
        {{ t("clusters.add.submit") }}
      </button>
    </template>
  </Modal>
</template>
