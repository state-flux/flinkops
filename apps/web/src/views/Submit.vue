<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import Tabs from "../components/Tabs.vue";
import {
  auth,
  editor,
  jobs,
  profiles,
  prom,
  sessions,
  templates,
  type Finding,
} from "../api/client";
import { notify } from "../lib/notify";
import { useWritable } from "../lib/session";
import { t } from "../lib/i18n";

const router = useRouter();
const mode = ref("wizard");
const step = ref(1);
const { readonly } = useWritable();
const STEPS = [
  { n: 1, label: t("submit.step.app") },
  { n: 2, label: t("submit.step.res") },
  { n: 3, label: t("submit.step.preview") },
];

const q = useQuery({ queryKey: ["profiles"], queryFn: profiles.list });
const me = useQuery({ queryKey: ["me"], queryFn: auth.me });
const clusters = computed(() => me.data?.value?.clusters ?? []);
const profileList = computed(
  () => (q.data?.value?.items ?? []) as { id: string; flinkVersion?: string }[],
);

const form = ref({
  name: "",
  namespace: "",
  cluster: "",
  profileId: "",
  deployMode: "application" as "application" | "session",
  jarURI: "",
  entryClass: "",
  args: "",
  parallelism: 1,
  tmReplicas: 1,
  slots: 1,
  jmCpu: 0.5,
  jmMem: 1024,
  tmCpu: 0.5,
  tmMem: 2048,
  ckptInterval: "5 min",
  upgradeMode: "savepoint" as "savepoint" | "last-state" | "stateless",
  labels: "team=data,tier=normal,env=prod",
});

const out = ref<{ yaml: string; spec: unknown; findings: Finding[] } | null>(null);

// ---- 从模板 / 粘贴 YAML / GitOps ----
const tplQ = useQuery({ queryKey: ["templates"], queryFn: templates.list });
const tplList = computed(() => tplQ.data?.value?.items ?? []);
const tplId = ref("");
const tplName = ref("");
const tplNs = ref("");
/** 模板/粘贴两条路径最终都落到这段 YAML 上，提交与校验共用 */
const yamlText = ref("");
const yamlFindings = ref<Finding[]>([]);
const yamlMsg = ref("");
/** 结果行是否失败态：靠状态而不是匹配中文（切语言后中文会变） */
const yamlDanger = ref(false);
const yamlBusy = ref(false);

const SAMPLE_YAML = `apiVersion: flink.apache.org/v1beta1
kind: FlinkDeployment
metadata:
  name: demo-job
  namespace: flink
spec:
  image: registry.example.com/flink/flink-base:1.17
  flinkVersion: v1_17
  serviceAccount: flink
  jobManager:
    resource: { cpu: 0.5, memory: "1024m" }
  taskManager:
    replicas: 1
    resource: { cpu: 0.5, memory: "2048m" }
  job:
    jarURI: local:///opt/flink/usrlib/app.jar
    parallelism: 1
    upgradeMode: savepoint
  flinkConfiguration:
    taskmanager.numberOfTaskSlots: "1"
    execution.checkpointing.interval: 5 min
`;

const yamlHasErr = computed(() => yamlFindings.value.some((f) => f.severity === "error"));

/** 用模板的 payload 叠在基线上渲染成完整 manifest */
async function renderFromTemplate() {
  const tpl = tplList.value.find((x) => x.id === tplId.value);
  if (!tpl) {
    notify(t("submit.pickTemplate"), "warn");
    return;
  }
  yamlBusy.value = true;
  yamlMsg.value = "";
  yamlDanger.value = false;
  try {
    const payload = (tpl.payload ?? {}) as Record<string, unknown>;
    const overrides: Record<string, unknown> = {
      ...payload,
      metadata: {
        ...(payload.metadata as Record<string, unknown> | undefined),
        ...(tplName.value ? { name: tplName.value } : {}),
        ...(tplNs.value ? { namespace: tplNs.value } : {}),
      },
    };
    const r = await editor.render(form.value.profileId || undefined, overrides);
    yamlText.value = r.yaml;
    yamlFindings.value = r.findings;
    yamlDanger.value = false;
    yamlMsg.value = t("submit.templateRendered", { name: tpl.name });
  } catch (e) {
    yamlDanger.value = true;
    yamlMsg.value = t("submit.renderFailed", { err: (e as Error).message });
  } finally {
    yamlBusy.value = false;
  }
}

async function validateYaml() {
  if (!yamlText.value.trim()) {
    notify(t("submit.needYaml"), "warn");
    return;
  }
  yamlBusy.value = true;
  yamlMsg.value = "";
  yamlDanger.value = false;
  try {
    const r = await editor.validate(yamlText.value);
    yamlFindings.value = r.findings;
    yamlDanger.value = r.hasErrors;
    yamlMsg.value = r.hasErrors
      ? t("submit.validationFailed", {
          n: r.findings.filter((f) => f.severity === "error").length,
        })
      : t("submit.validationOk");
  } catch (e) {
    yamlDanger.value = true;
    yamlMsg.value = t("submit.validationError", { err: (e as Error).message });
  } finally {
    yamlBusy.value = false;
  }
}

async function submitYaml() {
  if (!yamlText.value.trim()) {
    notify(t("submit.nothingToSubmit"), "warn");
    return;
  }
  const target = form.value.cluster || clusters.value[0] || "";
  if (
    !(await confirmDialog({
      message: t("submit.submitConfirm", { target }),
    }))
  )
    return;
  yamlBusy.value = true;
  yamlMsg.value = "";
  yamlDanger.value = false;
  try {
    const r = await jobs.create(yamlText.value, {
      cluster: form.value.cluster || undefined,
      dryRun: false,
    });
    yamlDanger.value = false;
    yamlMsg.value = t("submit.submitted", { cluster: r.cluster, ns: r.namespace, name: r.name });
    notify(yamlMsg.value, "ok");
    await router.push(`/jobs/${r.cluster}/${r.namespace}/${r.name}`);
  } catch (e) {
    yamlDanger.value = true;
    yamlMsg.value = t("submit.submitFailed", { err: (e as Error).message });
  } finally {
    yamlBusy.value = false;
  }
}

/** GitOps 模式：不接管 Git，只负责产出可直接提交的清单 */
const gitopsYaml = computed(() => out.value?.yaml ?? "");
function copyGitops() {
  const text = gitopsYaml.value || yamlText.value;
  if (!text) {
    notify(t("submit.needManifest"), "warn");
    return;
  }
  navigator.clipboard?.writeText(text).then(
    () => notify(t("submit.copied"), "ok"),
    () => notify(t("submit.copyFailed"), "warn"),
  );
}
function downloadGitops() {
  const text = gitopsYaml.value || yamlText.value;
  if (!text) {
    notify(t("submit.needManifest"), "warn");
    return;
  }
  const blob = new Blob([text], { type: "text/yaml;charset=utf-8" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = `${form.value.name || "flinkdeployment"}.yaml`;
  a.click();
  URL.revokeObjectURL(a.href);
  notify(t("submit.downloaded"), "ok");
}
// form 一改，之前"生成 spec"的结果就过期了：不失效的话第 3 步会提交旧配置
// （步骤条可以直接跳转，绕过 preview）。
// 切到模板 / 粘贴页时给一份可用初值，避免空白
watch(mode, (m) => {
  if (m === "template" && !yamlText.value) tplQ.refetch();
  if (m === "paste" && !yamlText.value) yamlText.value = "";
});

watch(
  form,
  () => {
    out.value = null;
  },
  { deep: true },
);
const busy = ref(false);
const err = ref("");
const result = ref("");
/** 结果行是失败态吗：靠状态而不是匹配中文前缀（切语言后前缀会变） */
const resultDanger = ref(false);

const canNext1 = computed(
  () =>
    !!form.value.name.trim() && (form.value.deployMode === "session" || !!form.value.jarURI.trim()),
);

// ---------------------------------------------------------------- 资源估算（呼应"提交前判定剩余资源"）
const clusterUsage = useQuery({
  queryKey: ["submit", "usage"],
  queryFn: async () => {
    const [capCpu, capMem, reqCpu, reqMem] = await Promise.all([
      prom.query('sum(kube_node_status_allocatable{resource="cpu"})').catch(() => null),
      prom.query('sum(kube_node_status_allocatable{resource="memory"})').catch(() => null),
      prom.query('sum(kube_pod_container_resource_requests{resource="cpu"})').catch(() => null),
      prom.query('sum(kube_pod_container_resource_requests{resource="memory"})').catch(() => null),
    ]);
    const num = (r: typeof capCpu) => Number(r?.data?.result?.[0]?.value?.[1] ?? NaN);
    return {
      capCpu: num(capCpu),
      capMem: num(capMem) / 1024 ** 3,
      reqCpu: num(reqCpu),
      reqMem: num(reqMem) / 1024 ** 3,
    };
  },
  retry: false,
});

const need = computed(() => {
  const f = form.value;
  return {
    cpu: Number(f.jmCpu) + Number(f.tmCpu) * Number(f.tmReplicas),
    mem: (Number(f.jmMem) + Number(f.tmMem) * Number(f.tmReplicas)) / 1024,
  };
});
const free = computed(() => {
  const u = clusterUsage.data?.value;
  if (!u) return { cpu: NaN, mem: NaN };
  return { cpu: u.capCpu - u.reqCpu, mem: u.capMem - u.reqMem };
});
const fits = computed(() => {
  const f = free.value;
  if (!Number.isFinite(f.cpu)) return null;
  return need.value.cpu <= f.cpu && need.value.mem <= f.mem;
});

const objects = computed(() => {
  const n = form.value.name || "<name>";
  const ns = form.value.namespace || "<baseline-ns>";
  if (form.value.deployMode === "session") {
    return [
      { kind: "FlinkDeployment", name: n, note: t("submit.obj.session") },
      { kind: "Deployment", name: `${n}`, note: "JobManager" },
      {
        kind: "Deployment",
        name: `${n}-taskmanager`,
        note: `TaskManager × ${form.value.tmReplicas}`,
      },
      { kind: "Service", name: `${n}-rest`, note: "REST 8081" },
      { kind: "ConfigMap", name: `flink-config-${n}`, note: "flink-conf.yaml" },
    ];
  }
  return [
    {
      kind: "FlinkDeployment",
      name: n,
      note: t("submit.obj.app", { p: form.value.parallelism }),
    },
    { kind: "Deployment", name: n, note: t("submit.obj.jm") },
    {
      kind: "Deployment",
      name: `${n}-taskmanager`,
      note: `TaskManager × ${form.value.tmReplicas}`,
    },
    { kind: "Service", name: `${n}-rest`, note: t("submit.obj.rest") },
    { kind: "ConfigMap", name: `flink-config-${n}`, note: "flink-conf.yaml" },
  ].map((o) => ({ ...o, ns }));
});

function labelMap() {
  const m: Record<string, string> = {};
  for (const kv of form.value.labels.split(",")) {
    const [k, v] = kv.split("=").map((s) => s.trim());
    if (k && v) m[k] = v;
  }
  return m;
}

async function preview() {
  busy.value = true;
  err.value = "";
  out.value = null;
  try {
    const overrides: Record<string, unknown> = {
      metadata: {
        name: form.value.name,
        namespace: form.value.namespace || undefined,
        labels: labelMap(),
      },
      job: {
        jarURI: form.value.jarURI,
        entryClass: form.value.entryClass || undefined,
        parallelism: Number(form.value.parallelism) || 1,
        args: form.value.args ? form.value.args.split(/\s+/).filter(Boolean) : [],
        upgradeMode: form.value.upgradeMode,
      },
      // TM 资源必须真的进 spec：之前这里只算"估算"，落库的是基线兜底的 2C/4Gi
      taskManager: {
        replicas: Number(form.value.tmReplicas) || 1,
        resource: {
          cpu: Number(form.value.tmCpu) || 1,
          memory: `${Number(form.value.tmMem) || 4096}m`,
        },
      },
      jobManager: { resource: { cpu: Number(form.value.jmCpu), memory: `${form.value.jmMem}m` } },
      flinkConfiguration: {
        "taskmanager.numberOfTaskSlots": String(form.value.slots || 1),
        "execution.checkpointing.interval": form.value.ckptInterval,
      },
    };
    out.value = await editor.render(form.value.profileId || undefined, overrides);
    step.value = 3;
  } catch (e) {
    err.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

const hasErr = computed(() => (out.value?.findings ?? []).some((f) => f.severity === "error"));

async function precheck() {
  if (!out.value) return;
  busy.value = true;
  result.value = "";
  try {
    const r = await jobs.create(out.value.yaml, {
      cluster: form.value.cluster || undefined,
      dryRun: true,
    });
    resultDanger.value = false;
    result.value = t("submit.precheckOk", { cluster: r.cluster, ns: r.namespace, name: r.name });
    notify(t("submit.precheckTitle"), "ok");
  } catch (e) {
    resultDanger.value = true;
    result.value = t("submit.precheckFail", { err: (e as Error).message });
  } finally {
    busy.value = false;
  }
}

async function submit() {
  const target = form.value.cluster || clusters.value[0] || "";
  if (
    (await Promise.resolve(true)) &&
    !(await confirmDialog({
      message: t("submit.confirmCreate", {
        target,
        kind: form.value.deployMode === "session" ? t("submit.kindSession") : "FlinkDeployment",
      }),
    }))
  )
    return;
  busy.value = true;
  result.value = "";
  try {
    if (form.value.deployMode === "session") {
      const r = await sessions.create({
        name: form.value.name,
        cluster: form.value.cluster || undefined,
        profileId: form.value.profileId || undefined,
        namespace: form.value.namespace || undefined,
        slots: form.value.slots,
        taskManagerReplicas: form.value.tmReplicas,
      });
      result.value = t("submit.sessionCreated", { ns: r.namespace, name: r.name });
      notify(result.value, "ok");
    } else {
      if (!out.value) return;
      const r = await jobs.create(out.value.yaml, { cluster: form.value.cluster || undefined });
      resultDanger.value = false;
      result.value = t("submit.submittedShort", {
        cluster: r.cluster,
        ns: r.namespace,
        name: r.name,
      });
      notify(result.value, "ok");
      await router.push(`/jobs/${r.cluster}/${r.namespace}/${r.name}`);
    }
  } catch (e) {
    result.value = t("submit.submitFailShort", { err: (e as Error).message });
  } finally {
    busy.value = false;
  }
}

const copyYaml = () => {
  const text = out.value?.yaml ?? "";
  void navigator.clipboard?.writeText(`cat <<'EOF' | kubectl apply -f -\n${text}\nEOF`);
  notify(t("submit.copiedKubectl"), "ok");
};
</script>

<template>
  <PageHead
    :title="t('page.submit')"
    :sub="t('submit.sub')"
    :badge="t('submit.badge')"
    tone="info"
  />

  <Tabs
    v-model="mode"
    :tabs="[
      { key: 'wizard', label: t('submit.tab.wizard') },
      { key: 'template', label: t('submit.tab.template') },
      { key: 'paste', label: t('submit.tab.paste') },
      { key: 'gitops', label: t('submit.tab.gitops') },
    ]"
  />

  <!-- 从模板：选模板 → 渲染成完整 manifest → 校验 / 提交 -->
  <div v-if="mode === 'template'" class="mt-3 grid gap-3 lg:grid-cols-[1fr_320px]">
    <Card :title="t('submit.tpl.title')" :hint="t('submit.tpl.hint')">
      <div class="flex flex-col gap-2 text-xs">
        <label class="flex flex-col gap-1">
          <span class="text-fg-2">{{ t("submit.tpl.label") }}</span>
          <select
            v-model="tplId"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-2xs"
          >
            <option value="">{{ t("submit.tpl.placeholder") }}</option>
            <option v-for="tpl in tplList" :key="tpl.id" :value="tpl.id">
              {{ tpl.name }} · {{ tpl.kind }}
            </option>
          </select>
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-fg-2">{{ t("submit.tpl.name") }}</span>
          <input
            v-model="tplName"
            placeholder="demo-job"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-2xs"
          />
        </label>
        <label class="flex flex-col gap-1">
          <span class="text-fg-2">{{ t("submit.tpl.ns") }}</span>
          <input
            v-model="tplNs"
            placeholder="flink"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-2xs"
          />
        </label>
        <p v-if="!tplList.length" class="text-2xs text-fg-2">
          {{ t("submit.tpl.emptyPrefix")
          }}<RouterLink to="/templates" class="underline">{{
            t("submit.tpl.emptyLink")
          }}</RouterLink
          >{{ t("submit.tpl.emptySuffix") }}
        </p>
        <div>
          <button
            :disabled="yamlBusy || !tplId"
            class="rounded-lg bg-primary px-3 py-1 text-2xs font-medium text-white disabled:opacity-50"
            @click="renderFromTemplate"
          >
            {{ t("submit.tpl.render") }}
          </button>
        </div>
      </div>
    </Card>

    <Card :title="t('submit.tplNotes.title')">
      <ul class="flex flex-col gap-1.5 text-2xs text-fg-2">
        <li>{{ t("submit.tplNotes.1") }}</li>
        <li>{{ t("submit.tplNotes.2") }}</li>
        <li>{{ t("submit.tplNotes.3") }}</li>
      </ul>
    </Card>
  </div>

  <!-- 模板渲染出的 manifest：可校验、可直接提交 -->
  <div v-if="mode === 'template' && yamlText" class="mt-3">
    <Card :title="t('submit.yaml.title')" :hint="t('submit.yaml.hint')">
      <template #actions>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="
            yamlText = '';
            yamlFindings = [];
            yamlMsg = '';
          "
        >
          {{ t("submit.clear") }}
        </button>
        <button
          :disabled="yamlBusy"
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover disabled:opacity-50"
          @click="validateYaml"
        >
          {{ t("submit.validate") }}
        </button>
        <button
          :disabled="yamlBusy || yamlHasErr || readonly"
          class="rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white disabled:opacity-50"
          @click="submitYaml"
        >
          {{ t("submit.submitTo") }}
        </button>
      </template>
      <pre
        class="max-h-[360px] overflow-auto rounded-lg bg-surface-2 p-3 font-mono text-2xs leading-relaxed text-fg-1"
        >{{ yamlText }}</pre>
    </Card>
  </div>

  <!-- 粘贴 YAML：直接提交一份完整 manifest -->
  <div v-if="mode === 'paste'" class="mt-3">
    <Card :title="t('submit.paste.title')" :hint="t('submit.paste.hint')">
      <template #actions>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="yamlText = SAMPLE_YAML"
        >
          {{ t("submit.insertSample") }}
        </button>
        <button
          :disabled="yamlBusy"
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover disabled:opacity-50"
          @click="validateYaml"
        >
          {{ t("submit.validate") }}
        </button>
        <button
          :disabled="yamlBusy || yamlHasErr || readonly"
          class="rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white disabled:opacity-50"
          @click="submitYaml"
        >
          {{ t("submit.submitTo") }}
        </button>
      </template>
      <textarea
        v-model="yamlText"
        rows="18"
        spellcheck="false"
        placeholder="apiVersion: flink.apache.org/v1beta1&#10;kind: FlinkDeployment&#10;metadata:&#10;  name: demo-job&#10;  namespace: flink&#10;spec:&#10;  ..."
        class="w-full rounded-lg border border-line bg-surface-2 p-3 font-mono text-2xs leading-relaxed"
      ></textarea>
    </Card>
  </div>

  <!-- GitOps：产出可直接进仓库的清单（平台不接管 Git） -->
  <div v-if="mode === 'gitops'" class="mt-3 grid gap-3 lg:grid-cols-[1fr_320px]">
    <Card :title="t('submit.gitops.title')" :hint="t('submit.gitops.hint')">
      <template #actions>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="copyGitops"
        >
          {{ t("submit.gitops.copy") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="downloadGitops"
        >
          {{ t("submit.gitops.download") }}
        </button>
      </template>
      <pre
        v-if="gitopsYaml || yamlText"
        class="max-h-[420px] overflow-auto rounded-lg bg-surface-2 p-3 font-mono text-2xs leading-relaxed text-fg-1"
        >{{ gitopsYaml || yamlText }}</pre>
      <p v-else class="text-xs text-fg-2">{{ t("submit.gitops.empty") }}</p>
    </Card>
    <Card :title="t('submit.gitops.how')">
      <ol class="flex flex-col gap-1.5 text-2xs text-fg-2">
        <li>
          {{ t("submit.gitops.step1") }}<span class="font-mono">deploy/</span
          >{{ t("submit.gitops.step1Tail") }}
        </li>
        <li>{{ t("submit.gitops.step2") }}<span class="font-mono">kubectl apply -f</span></li>
        <li>{{ t("submit.gitops.step3") }}</li>
      </ol>
      <p class="mt-2 rounded-lg bg-surface-2 px-2.5 py-2 text-3xs text-fg-2">
        {{ t("submit.gitops.note") }}
      </p>
    </Card>
  </div>

  <!-- 模板 / 粘贴 / GitOps 都要能就地校验并看到结果 -->
  <div
    v-if="mode !== 'wizard' && (yamlText || yamlMsg)"
    class="mt-3 rounded-xl border border-line-2 bg-surface px-4 py-3 text-xs"
  >
    <div class="flex flex-wrap items-center gap-2">
      <span class="text-fg-2">{{ t("submit.result") }}</span>
      <span v-if="yamlMsg" :class="yamlDanger || yamlHasErr ? 'text-danger' : 'text-ok'">{{
        yamlMsg
      }}</span>
      <Badge v-if="yamlFindings.length" tone="warn">{{
        t("submit.findingsSuffix", { n: yamlFindings.length })
      }}</Badge>
    </div>
    <ul v-if="yamlFindings.length" class="mt-2 flex flex-col gap-1">
      <li
        v-for="(f, i) in yamlFindings.slice(0, 12)"
        :key="i"
        class="text-2xs"
        :class="f.severity === 'error' ? 'text-danger' : 'text-warn'"
      >
        <span class="font-mono text-3xs">{{ f.path }}</span> — {{ f.message }}
      </li>
    </ul>
  </div>

  <template v-if="mode === 'wizard'">
    <!-- 步骤条 -->
    <div
      class="mb-3 flex items-center gap-2 rounded-xl border border-line-2 bg-surface px-4 py-2.5"
    >
      <div v-for="s in STEPS" :key="s.n" class="flex items-center gap-2">
        <button
          class="flex h-6 w-6 items-center justify-center rounded-full text-2xs font-medium"
          :class="
            step === s.n
              ? 'bg-primary text-white'
              : step > s.n
                ? 'bg-ok/15 text-ok'
                : 'bg-surface-3 text-fg-2'
          "
          @click="step = s.n"
        >
          {{ step > s.n ? "✓" : s.n }}
        </button>
        <span class="text-xs" :class="step === s.n ? 'font-medium' : 'text-fg-2'">{{
          s.label
        }}</span>
        <span v-if="s.n < 3" class="mx-1 h-px w-8 bg-line"></span>
      </div>
      <div class="ml-auto flex gap-2">
        <button
          :disabled="step === 1"
          class="rounded-lg border border-line px-2.5 py-1 text-xs disabled:opacity-40"
          @click="step -= 1"
        >
          {{ t("submit.prev") }}
        </button>
        <button
          v-if="step < 3"
          :disabled="step === 1 ? !canNext1 : false"
          class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
          @click="step === 2 ? preview() : (step += 1)"
        >
          {{ step === 2 ? (busy ? t("submit.rendering") : t("submit.genSpec")) : t("submit.next") }}
        </button>
      </div>
    </div>
    <p v-if="err && step !== 3" class="mt-2 rounded-lg bg-danger/10 px-3 py-2 text-xs text-danger">
      {{ err }}
    </p>

    <!-- 步骤 1 -->
    <div v-if="step === 1" class="grid gap-3 xl:grid-cols-[minmax(0,1fr)_340px]">
      <Card :title="t('submit.step1.title')" :hint="t('submit.step1.hint')">
        <div
          class="grid gap-3 text-xs"
          style="grid-template-columns: repeat(auto-fit, minmax(210px, 1fr))"
        >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.name") }}</span>
            <input
              v-model="form.name"
              placeholder="orders-etl"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.ns") }}</span>
            <input
              v-model="form.namespace"
              :placeholder="t('submit.step1.nsPlaceholder')"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.cluster") }}</span>
            <select
              v-model="form.cluster"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
            >
              <option value="">{{ t("submit.step1.clusterAuto") }}</option>
              <option v-for="c in clusters" :key="c" :value="c">{{ c }}</option>
            </select></label
          >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.profile") }}</span>
            <select
              v-model="form.profileId"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
            >
              <option value="">{{ t("submit.step1.profileDefault") }}</option>
              <option v-for="p in profileList" :key="p.id" :value="p.id">
                {{ p.id }}（{{ p.flinkVersion }}）
              </option>
            </select></label
          >
        </div>

        <div class="mt-3">
          <div class="mb-1 text-2xs text-fg-2">{{ t("submit.step1.mode") }}</div>
          <div class="inline-flex overflow-hidden rounded-lg border border-line text-2xs">
            <button
              class="px-3 py-1"
              :class="
                form.deployMode === 'application'
                  ? 'bg-primary text-white'
                  : 'bg-surface-2 text-fg-2 hover:text-fg'
              "
              @click="form.deployMode = 'application'"
            >
              {{ t("submit.step1.modeApp") }}
            </button>
            <button
              class="px-3 py-1"
              :class="
                form.deployMode === 'session'
                  ? 'bg-primary text-white'
                  : 'bg-surface-2 text-fg-2 hover:text-fg'
              "
              @click="form.deployMode = 'session'"
            >
              {{ t("submit.step1.modeSession") }}
            </button>
          </div>
        </div>

        <div
          v-if="form.deployMode === 'application'"
          class="mt-3 grid gap-3 text-xs"
          style="grid-template-columns: repeat(auto-fit, minmax(210px, 1fr))"
        >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">JAR URI *</span>
            <input
              v-model="form.jarURI"
              placeholder="local:///opt/flink/usrlib/app.jar"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.entry") }}</span>
            <input
              v-model="form.entryClass"
              placeholder="com.example.Job"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="col-span-full flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step1.args") }}</span>
            <input
              v-model="form.args"
              placeholder="-t orders --env prod"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
        </div>
        <p v-else class="mt-3 rounded-lg bg-surface-2 px-3 py-2 text-2xs text-fg-2">
          {{ t("submit.step1.sessionNote") }}
        </p>
      </Card>

      <Card :title="t('submit.step1.labels')" hint="metadata.labels">
        <label class="flex flex-col gap-1 text-xs"
          ><span class="text-fg-2">{{ t("submit.step1.labelsHint") }}</span>
          <input
            v-model="form.labels"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-2xs"
        /></label>
        <div class="mt-2 flex flex-wrap gap-1">
          <span
            v-for="(v, k) in labelMap()"
            :key="k"
            class="rounded-full border border-line bg-surface-3 px-1.5 py-0.5 text-3xs text-fg-2"
            >{{ k }}={{ v }}</span
          >
        </div>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("submit.step1.labelsNote") }}<span class="font-mono">team / tier / env</span
          >{{ t("submit.step1.labelsNoteTail") }}
        </p>
      </Card>
    </div>

    <!-- 步骤 2 -->
    <div v-else-if="step === 2" class="grid gap-3 xl:grid-cols-[minmax(0,1fr)_360px]">
      <Card :title="t('submit.step2.title')">
        <div
          class="grid gap-3 text-xs"
          style="grid-template-columns: repeat(auto-fit, minmax(150px, 1fr))"
        >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.parallelism") }}</span>
            <input
              v-model.number="form.parallelism"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.tmReplicas") }}</span>
            <input
              v-model.number="form.tmReplicas"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.slots") }}</span>
            <input
              v-model.number="form.slots"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.jmCpu") }}</span>
            <input
              v-model.number="form.jmCpu"
              type="number"
              min="0.1"
              step="0.1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.jmMem") }}</span>
            <input
              v-model.number="form.jmMem"
              type="number"
              min="256"
              step="256"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.tmCpu") }}</span>
            <input
              v-model.number="form.tmCpu"
              type="number"
              min="0.1"
              step="0.1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.tmMem") }}</span>
            <input
              v-model.number="form.tmMem"
              type="number"
              min="512"
              step="256"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.ckpt") }}</span>
            <select
              v-model="form.ckptInterval"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
            >
              <option value="1 min">1 min</option>
              <option value="3 min">3 min</option>
              <option value="5 min">5 min</option>
              <option value="10 min">10 min</option>
            </select></label
          >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("submit.step2.upgrade") }}</span>
            <select
              v-model="form.upgradeMode"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
            >
              <option value="savepoint">{{ t("submit.upgrade.savepoint") }}</option>
              <option value="last-state">last-state</option>
              <option value="stateless">{{ t("submit.upgrade.stateless") }}</option>
            </select></label
          >
        </div>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("submit.slotsLineBold") }}
          <b class="font-mono">{{ form.tmReplicas * form.slots }}</b>{{
            t("submit.slotsLineTail", { p: form.parallelism })
          }}
        </p>
      </Card>

      <Card
        :title="t('submit.estimateCard')"
        :hint="
          fits === null
            ? t('submit.promDown')
            : fits
              ? t('submit.canSubmit')
              : t('submit.notEnough')
        "
      >
        <div class="flex flex-col gap-2 text-xs">
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("submit.requests") }}</span><span class="font-mono"
              >{{ need.cpu.toFixed(2) }} {{ t("submit.cores") }} /
              {{ need.mem.toFixed(1) }} GiB</span
            >
          </div>
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("submit.clusterFree") }}</span><span class="font-mono"
              >{{ Number.isFinite(free.cpu) ? free.cpu.toFixed(1) : "—" }} {{ t("submit.cores") }} /
              {{ Number.isFinite(free.mem) ? free.mem.toFixed(0) : "—" }} GiB</span
            >
          </div>
          <div class="mt-1 h-[7px] overflow-hidden rounded bg-surface-3">
            <i
              class="block h-full"
              :style="{
                width:
                  Math.min(
                    100,
                    Number.isFinite(free.cpu) && free.cpu > 0
                      ? (need.cpu / (free.cpu + need.cpu)) * 100
                      : 0,
                  ) + '%',
                background: fits === false ? 'var(--color-danger)' : 'var(--color-primary)',
              }"
            ></i>
          </div>
          <div
            class="mt-1 rounded-lg px-3 py-2 text-2xs"
            :class="
              fits === false
                ? 'bg-danger/10 text-danger'
                : fits
                  ? 'bg-ok/10 text-ok'
                  : 'bg-warn/10 text-warn'
            "
          >
            <template v-if="fits === false">
              {{
                t("submit.shortageLine", {
                  cpu: (need.cpu - free.cpu).toFixed(2),
                  mem: (need.mem - free.mem).toFixed(1),
                })
              }}
            </template>
            <template v-else-if="fits">{{ t("submit.fits") }}</template>
            <template v-else>{{ t("submit.noMetrics") }}</template>
          </div>
          <p class="text-3xs leading-relaxed text-fg-2">
            {{ t("submit.metricsCaveat") }}
          </p>
        </div>
      </Card>
    </div>

    <!-- 步骤 3 -->
    <div v-else class="grid gap-3 xl:grid-cols-[minmax(0,1fr)_360px]">
      <div class="flex flex-col gap-3">
        <Card v-if="out" :title="t('submit.validationCard')" :hint="`${out.findings.length} 条`">
          <div class="flex flex-wrap gap-1.5">
            <Badge v-if="!out.findings.length" tone="ok" dot>{{ t("submit.passed") }}</Badge>
            <Badge
              v-for="(f, i) in out.findings"
              :key="i"
              :tone="f.severity === 'error' ? 'danger' : f.severity === 'warn' ? 'warn' : 'idle'"
              >{{ f.code }}</Badge
            >
          </div>
          <ul class="mt-2 flex flex-col gap-1 text-2xs">
            <li
              v-for="(f, i) in out.findings"
              :key="i"
              :class="
                f.severity === 'error'
                  ? 'text-danger'
                  : f.severity === 'warn'
                    ? 'text-warn'
                    : 'text-fg-2'
              "
            >
              <span class="font-mono text-3xs">{{ f.path }}</span> — {{ f.message }}
            </li>
          </ul>
        </Card>
        <p v-if="err" class="rounded-lg bg-danger/10 px-3 py-2 text-xs text-danger">{{ err }}</p>

        <Card v-if="out" :title="t('submit.yamlCard')" :hint="t('submit.yamlCardHint')">
          <template #actions>
            <button
              class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
              @click="copyYaml"
            >
              {{ t("submit.copyKubectl") }}
            </button>
            <button
              :disabled="busy || readonly"
              class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover disabled:opacity-50"
              @click="precheck"
            >
              {{ t("submit.dryRun") }}
            </button>
            <button
              :disabled="busy || hasErr || readonly"
              class="rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white disabled:opacity-50"
              @click="submit"
            >
              {{ t("submit.submitTo") }}
            </button>
          </template>
          <pre
            class="max-h-[380px] overflow-auto rounded-lg bg-surface-2 p-3 font-mono text-2xs leading-relaxed text-fg-1"
            >{{ out.yaml }}</pre>
          <p
            v-if="result"
            :class="[
              'mt-2 rounded-lg px-3 py-2 text-2xs',
              resultDanger ? 'bg-danger/10 text-danger' : 'bg-ok/10 text-ok',
            ]"
          >
            {{ result }}
          </p>
        </Card>

        <Card v-else :title="t('submit.noSpec')" :hint="t('submit.noSpecHint')">
          <button class="rounded-lg bg-primary px-3 py-1 text-xs text-white" @click="step = 2">
            {{ t("submit.backToStep2") }}
          </button>
        </Card>
      </div>

      <div class="flex flex-col gap-3">
        <Card :title="t('submit.objectsCard')" :pad="false">
          <div class="divide-y divide-line-2/70">
            <div v-for="(o, i) in objects" :key="i" class="px-4 py-2">
              <div class="flex items-center gap-2">
                <span
                  class="rounded border border-line px-1.5 py-0.5 font-mono text-3xs text-fg-2"
                  >{{ o.kind }}</span
                >
                <span class="truncate font-mono text-2xs">{{ o.name }}</span>
              </div>
              <div class="mt-0.5 text-3xs text-fg-2">{{ o.note }}</div>
            </div>
          </div>
          <p class="border-t border-line-2 px-4 py-2 text-3xs text-fg-2">
            {{ t("submit.objectsNote") }}
          </p>
        </Card>

        <Card :title="t('submit.checksCard')">
          <ul class="flex flex-col gap-1.5 text-2xs">
            <li :class="fits === false ? 'text-danger' : 'text-ok'">
              {{ t("submit.check.resources") }}{{ fits === false ? "不足" : fits ? "够" : "未知" }}
            </li>
            <li :class="hasErr ? 'text-danger' : 'text-ok'">
              {{ t("submit.check.validation") }}{{ hasErr ? "有 error 级问题" : "无 error" }}
            </li>
            <li :class="!form.name ? 'text-warn' : 'text-ok'">
              {{ t("submit.check.naming") }}{{ form.name || "未填" }}
            </li>
            <li class="text-fg-2">{{ t("submit.check.visible") }}</li>
          </ul>
        </Card>
      </div>
    </div>
    <p v-if="err && step !== 3" class="mt-2 rounded-lg bg-danger/10 px-3 py-2 text-xs text-danger">
      {{ err }}
    </p>
  </template>
</template>
