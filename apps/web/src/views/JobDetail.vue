<script setup lang="ts">
import { confirmDialog, promptDialog } from "../lib/confirm";
import { computed, nextTick, ref, watch } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import Tabs from "../components/Tabs.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Badge from "../components/Badge.vue";
import { editor, jobs, request } from "../api/client";
import type { LooseJson, SnapshotItem } from "../api/types";
import LineChart from "../components/charts/LineChart.vue";
import HeatMap from "../components/charts/HeatMap.vue";
import PageActions from "../components/PageActions.vue";
import Modal from "../components/Modal.vue";
import { download, notify } from "../lib/notify";
import { useWritable } from "../lib/session";
import { useRouter } from "vue-router";
import { fmtLocal, utcOf } from "../lib/time";
import { t } from "../lib/i18n";

const p = defineProps<{ cluster: string; ns: string; name: string }>();
const router = useRouter();
// 只读模式下把所有写操作按钮禁用（后端也会 403，但别让用户白点）
const { readonly } = useWritable();
const acting = ref(false);
const tab = ref("overview");
const timeRange = ref("1h");
const bp = ref<Record<string, unknown> | null>(null);
const bpBusy = ref(false);
const fgOpen = ref(false);
const fgBusy = ref(false);
const fgData = ref<Record<string, unknown> | null>(null);
const ingOpen = ref(false);
const ingHost = ref("");
const delOpen = ref(false);
const delText = ref("");
const uiOpen = ref(false);
const logText = ref("");
const logErr = ref("");
const yamlText = ref("");
// 切到 YAML Tab 自动加载一次（原来要手动点「{{ t("jd.loadYaml") }}」，切过去是空白以为坏了）
watch(
  () => tab.value,
  (t) => {
    if (t === "yaml" && !yamlText.value) void loadYaml();
  },
);

const detail = useQuery({
  queryKey: ["job", p.cluster, p.ns, p.name],
  queryFn: () => jobs.detail(p.cluster, p.ns, p.name),
  refetchInterval: 15_000,
});
const metrics = useQuery({
  queryKey: ["metrics", p.cluster, p.ns, p.name],
  // 吞吐要两次采样之差：首次无基线，5s 轮询一次才有 per-second
  refetchInterval: 5_000,
  queryFn: () => jobs.liveMetrics(p.cluster, p.ns, p.name),
  enabled: computed(() => tab.value === "metrics"),
});

const events = useQuery({
  queryKey: ["events", p.cluster, p.ns, p.name],
  queryFn: () => jobs.events(p.cluster, p.ns, p.name),
  enabled: computed(() => tab.value === "events"),
});
const exceptions = useQuery({
  queryKey: ["exceptions", p.cluster, p.ns, p.name],
  queryFn: () => jobs.exceptions(p.cluster, p.ns, p.name),
  enabled: computed(() => tab.value === "exceptions"),
  retry: false,
});

// 平台快照（FlinkStateSnapshot）：与 operator 的 savepointHistory 是两套数据，
// 详情页只读 history 会出现"快照页有 4 条、这里 0 条"的错觉。
const snaps = useQuery({
  queryKey: ["snapshots", "job", p.cluster, p.ns, p.name],
  queryFn: async () => {
    const r = await request<{ items: SnapshotItem[] }>(
      `/api/snapshots?cluster=${encodeURIComponent(p.cluster)}&namespace=${encodeURIComponent(p.ns)}`,
    );
    return (r.items ?? []).filter(
      (x: SnapshotItem) =>
        x.jobName === p.name || (!x.jobName && String(x.name ?? "").startsWith(`${p.name}-`)),
    );
  },
  refetchInterval: 15_000,
  enabled: computed(() => tab.value === "snapshots"),
});
const snapRows = computed(() => (snaps.data?.value as SnapshotItem[]) ?? []);
const d = computed(() => detail.data?.value as LooseJson);
const asObj = (v: unknown): LooseJson => (v && typeof v === "object" ? (v as LooseJson) : {});
const s = computed<LooseJson>(() => asObj(d.value?.summary));

// 路由参数变了（同一条 /jobs/:c/:ns/:name 记录），组件实例会被复用，
// 页内本地状态必须清空：否则会把上一个作业的日志 / YAML / 背压矩阵
// 挂在新作业名下展示——排障时最容易读错的就是这个。
watch(
  () => [p.cluster, p.ns, p.name].join("/"),
  () => {
    tab.value = "overview";
    logText.value = "";
    logErr.value = "";
    yamlText.value = "";
    bp.value = null;
    bpHist.value = [];
    hist.value = [];
    acting.value = false;
  },
);
const spec = computed<LooseJson>(() => asObj(d.value?.spec));
// 模板里逐字段断言很难看：这里一次性窄化成渲染需要的形状
const ckView = computed(() => {
  const raw = ck.value;
  const last = (raw.last ?? {}) as LooseJson;
  return {
    completed: Number(raw.completed ?? 0),
    total: Number(raw.total ?? 0),
    failed: Number(raw.failed ?? 0),
    restored: Number(raw.restored ?? 0),
    lastMs: Number(last.durationMs ?? 0),
    lastId: String(last.id ?? ""),
    lastSize: last.size == null ? "—" : String(last.size),
    lastAt: last.timestamp as string | undefined,
    hasLast: Object.keys(last).length > 0,
  };
});
const spView = computed(() => {
  const raw = sp.value;
  const last = (raw.last ?? {}) as LooseJson;
  return {
    historyLen: ((raw.history ?? []) as unknown[]).length,
    lastPath: String(last.path ?? last.id ?? "—"),
    hasLast: Object.keys(last).length > 0,
    history: ((raw.history ?? []) as LooseJson[]).map((h) => ({
      id: String(h.id ?? ""),
      path: String(h.path ?? "—"),
      timestamp: h.timestamp as string | undefined,
    })),
  };
});
const conditions = computed(
  () => (s.value.conditions ?? []) as { type?: string; status?: string; reason?: string }[],
);
// 详情读的是实时 CRD：失败时必须显式说"加载失败"，否则 KPI 会渲染成 0/—，
// 运维会把它读成"作业没有数据"（比报错更贵）。
const loadErr = computed(() =>
  detail.isError.value ? ((detail.error.value as Error | null)?.message ?? t("jd.loadFailed")) : "",
);
const ck = computed<LooseJson>(() => asObj(d.value?.checkpoint));
const sp = computed<LooseJson>(() => asObj(d.value?.savepoint));
const mv = computed(() => (metrics.data?.value as LooseJson) ?? ({} as LooseJson));
const cfg = computed(
  () => Object.entries(spec.value?.flinkConfiguration ?? {}) as [string, string][],
);

// ---- 5 分钟环形缓冲（吞吐）：前端自己攒点，不依赖 Prometheus
const hist = ref<{ label: string; in: number; out: number }[]>([]);
watch(
  mv,
  (v) => {
    const vals = (v?.values ?? {}) as Record<string, number>;
    if (!Object.keys(vals).length) return;
    const d = new Date();
    const label = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
    hist.value = [
      ...hist.value.slice(-59),
      {
        label,
        in: Number(vals.numRecordsInPerSecond ?? 0),
        out: Number(vals.numRecordsOutPerSecond ?? 0),
      },
    ];
  },
  { deep: true },
);
const histLabels = computed(() => hist.value.map((h) => h.label));
const histSeries = computed(() => [
  { name: "In (rec/s)", data: hist.value.map((h) => h.in) },
  { name: "Out (rec/s)", data: hist.value.map((h) => h.out) },
]);

// ---- 背压矩阵：每次采样记一行（顶点/子任务 × 时间）
const bpHist = ref<{ rows: { label: string; value: number }[] }[]>([]);
const bpMatrix = computed(() => {
  const h = bpHist.value;
  if (!h.length) return [];
  const keys = [...new Set(h.flatMap((x) => x.rows.map((r) => r.label)))];
  return keys.map((k) => ({
    label: k,
    values: h.map((x) => x.rows.find((r) => r.label === k)?.value ?? 0),
  }));
});

// ---- 算子表（顶点 × 子任务聚合指标，走只读反代）
const jobIdRef = computed(() => s.value.jobId);
// 基本信息里的长值（镜像 / JAR / JobID）会被 truncate，点一下复制全文
async function copyText(v: string) {
  if (!v) return;
  try {
    await navigator.clipboard.writeText(v);
    notify(t("jd.copied"), "ok");
  } catch {
    notify(t("jd.copyBlocked"), "warn");
  }
}
const vertRows = useQuery({
  queryKey: ["verts", p.cluster, p.ns, p.name, jobIdRef],
  queryFn: async () => {
    const jid = String(s.value.jobId);
    const base = `/proxy/${p.cluster}/${p.ns}/${p.name}`;
    const job = await request<{ vertices?: { id: string; name: string; parallelism: number }[] }>(
      `${base}/jobs/${jid}`,
    );
    const out = [];
    for (const v of job.vertices ?? []) {
      const m = await request<{ id: string; sum?: number }[]>(
        `${base}/jobs/${jid}/vertices/${v.id}/subtasks/metrics?get=numRecordsInPerSecond,numRecordsOutPerSecond,busyTimeMsPerSecond,backPressuredTimeMsPerSecond`,
      ).catch(() => []);
      const sum = (id: string) => Number(m.find((x) => x.id === id)?.sum ?? 0);
      out.push({
        id: v.id,
        name: v.name,
        parallelism: v.parallelism,
        inRate: sum("numRecordsInPerSecond"),
        outRate: sum("numRecordsOutPerSecond"),
        busy: sum("busyTimeMsPerSecond") / 10,
        bp: sum("backPressuredTimeMsPerSecond"),
      });
    }
    return out;
  },
  enabled: computed(() => tab.value === "metrics" && !!s.value.jobId),
  refetchInterval: 30_000,
  retry: false,
});

const tabs = computed(() => [
  { key: "overview", label: t("jd.tab.overview") },
  { key: "metrics", label: t("jd.tab.metrics") },
  { key: "ckpt", label: "Checkpoint", badge: Number(ck.value?.completed ?? 0) || "" },
  {
    key: "snapshots",
    label: t("jd.tab.snapshots"),
    badge: ((sp.value?.history as unknown[] | undefined)?.length ?? 0) || "",
  },
  { key: "exceptions", label: t("jd.tab.exceptions") },
  { key: "logs", label: t("jd.tab.logs") },
  { key: "config", label: t("jd.tab.config") },
  { key: "yaml", label: "YAML" },
  { key: "events", label: t("jd.tab.events") },
]);

async function act(kind: "restart" | "stop" | "start") {
  const label = {
    restart: t("jd.act.restart"),
    stop: t("jd.act.suspend"),
    start: t("jd.act.start"),
  }[kind];
  // 连点保护：按钮虽然会 disabled，但键盘回车/脚本仍可能二次触发
  if (acting.value) return;
  if (!(await confirmDialog({ message: t("jd.act.confirm", { name: p.name, action: label }) })))
    return;
  acting.value = true;
  try {
    if (kind === "restart") {
      const r = await jobs.restart(p.cluster, p.ns, p.name);
      notify(t("jd.restarted", { nonce: r.restartNonce ?? "" }), "ok");
    } else {
      await jobs.setState(p.cluster, p.ns, p.name, kind === "start");
      notify(t("jd.act.done", { action: label }), "ok");
    }
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    acting.value = false;
  }
}

// Savepoint：详情页也支持指定路径（默认取作业配置的 state.savepoints.dir + 时间戳）
const spOpen = ref(false);
const spPath = ref("");
function openSavepoint() {
  const dir = String(
    (spec.value as { flinkConfiguration?: Record<string, string> })?.flinkConfiguration?.[
      "state.savepoints.dir"
    ] ?? "",
  ).replace(/\/+$/, "");
  const stamp = new Date()
    .toISOString()
    .replace(/[-:TZ.]/g, "")
    .slice(0, 14);
  spPath.value = dir ? `${dir}/sp-${stamp}` : "";
  spOpen.value = true;
}
async function doSavepoint() {
  const path = spPath.value.trim();
  if (
    path &&
    !(await confirmDialog({
      message: t("jd.savepoint.overwrite", { path }),
      danger: true,
    }))
  )
    return;
  acting.value = true;
  try {
    const r = await jobs.savepoint(p.cluster, p.ns, p.name, path || undefined);
    if (r.mode === "rest") {
      notify(t("jd.savepoint.degraded", { id: r.requestId ?? "" }), "warn", 6000);
    } else {
      notify(
        t("jd.savepoint.done", { id: r.snapshot ?? "", state: r.state ? ` (${r.state})` : "" }),
        "ok",
        5000,
      );
    }
    spOpen.value = false;
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    acting.value = false;
  }
}

const headerActions = computed(() => [
  {
    label: s.value.state === "RUNNING" ? t("jd.act.suspend") : t("jd.act.start"),
    run: () => act(s.value.state === "RUNNING" ? "stop" : "start"),
  },
  { label: t("jd.act.restart"), run: () => act("restart") },
  { label: "Savepoint", run: () => openSavepoint(), primary: true },
  { label: t("jd.act.edit"), run: () => router.push(`/editor/${p.cluster}/${p.ns}/${p.name}`) },
]);

const fmt = (v?: unknown) => {
  const n = Number(v);
  if (v == null || !Number.isFinite(n)) return "—";
  return n >= 1000 ? `${(n / 1000).toFixed(1)}K` : String(Math.round(n));
};
// 时间统一走 lib/time：后端是 UTC，这里按浏览器本地时区显示
const ts = (v?: unknown) => fmtLocal(v);
// 悬停显示原始 UTC，跨时区对时间时不用心算
const tsTip = (v?: unknown) => utcOf(v);

async function sampleBackpressure() {
  bpBusy.value = true;
  try {
    const r = await jobs.backpressure(p.cluster, p.ns, p.name);
    bp.value = r;
    // 记一行到矩阵：顶点 + 子任务 busyRatio
    const subs =
      (r as { subtasks?: { subtask: number; busyRatio?: number; backpressureLevel?: string }[] })
        .subtasks ?? [];
    const vertex = String((r as { vertex?: string }).vertex ?? "").slice(0, 6);
    if (subs.length) {
      bpHist.value = [
        ...bpHist.value.slice(-15),
        {
          rows: subs.map((x) => ({
            label: `v${vertex}·st${x.subtask}`,
            value: Number(x.busyRatio ?? 0),
          })),
        },
      ];
    }
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    bpBusy.value = false;
  }
}

// 创建 UI Ingress：走页面内 Modal（原来用 prompt，无法展示"会暴露 Flink UI"的提示）
function createIngress() {
  ingHost.value = `flink-${p.name}.example.com`;
  ingOpen.value = true;
}

// 删除：页面内 Modal + 输入作业名（原 prompt 同样是原生弹窗）
// 取消（不保留状态）：upgradeMode -> stateless 再 suspend，不产生新的 savepoint
async function actCancel() {
  if (acting.value) return;
  if (
    !(await confirmDialog({
      title: t("jd.cancel.title"),
      message: t("jd.cancel.message", { name: p.name }),
      detail: t("jd.cancel.detail"),
      okText: t("jd.cancel.title"),
      danger: true,
    }))
  )
    return;
  acting.value = true;
  try {
    const r = await jobs.cancel(p.cluster, p.ns, p.name);
    notify(t("jd.cancelled", { mode: r.upgradeMode ?? "" }), "ok");
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    acting.value = false;
  }
}

// 回滚镜像：留空则用审计里的上一个镜像
async function doRollback() {
  if (acting.value) return;
  const img = await promptDialog({
    message: t("jd.rollback.prompt"),
    input: { value: "" },
  });
  if (img === null) return;
  acting.value = true;
  try {
    const r = await jobs.rollback(p.cluster, p.ns, p.name, img.trim() || undefined);
    notify(t("jd.rollback.done", { from: r.from ?? "", to: r.to ?? "" }), "ok");
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    acting.value = false;
  }
}

async function removeJob() {
  delOpen.value = true;
  delText.value = "";
}
async function confirmDelete() {
  if (delText.value.trim() !== p.name) {
    notify(t("jd.nameMismatch"), "warn");
    return;
  }
  try {
    await jobs.remove(p.cluster, p.ns, p.name);
    notify(t("jd.deleted", { name: p.name }), "ok");
    delOpen.value = false;
    await router.push("/jobs");
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

// 扩容
const scaleOpen = ref(false);
const scaleP = ref(1);
const scaleTm = ref(1);
function openScale() {
  scaleP.value = Number((spec.value as LooseJson)?.parallelism ?? 1) || 1;
  scaleTm.value = Number((spec.value as LooseJson)?.taskManagerReplicas ?? 1) || 1;
  scaleOpen.value = true;
}
const submitting = ref(false);
async function doScale() {
  if (submitting.value) return;
  if (
    !(await confirmDialog({
      message: t("jd.scale.confirm", { name: p.name, p: scaleP.value, tm: scaleTm.value }),
    }))
  )
    return;
  submitting.value = true;
  try {
    await jobs.scale(p.cluster, p.ns, p.name, {
      parallelism: scaleP.value,
      taskManagerReplicas: scaleTm.value,
    });
    notify(t("jd.scale.done", { p: scaleP.value, tm: scaleTm.value }), "ok");
    scaleOpen.value = false;
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    submitting.value = false;
  }
}

// 从 savepoint 恢复
async function doRestore(path?: string) {
  const target = path || t("jd.restore.latest");
  if (!(await confirmDialog({ message: t("jd.restore.confirm", { target, name: p.name }) })))
    return;
  if (submitting.value) return;
  submitting.value = true;
  try {
    const r = await jobs.restore(p.cluster, p.ns, p.name, path);
    notify(t("jd.restore.done", { from: r.from ?? target }), "ok");
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    submitting.value = false;
  }
}

// 滚动升级
const upOpen = ref(false);
const upImage = ref("");
async function doUpgrade() {
  if (!upImage.value.trim()) {
    notify(t("jd.upgrade.needImage"), "warn");
    return;
  }
  if (submitting.value) return;
  const cur = (spec.value as LooseJson)?.image ?? "—";
  if (
    !(await confirmDialog({
      message: t("jd.upgrade.confirm", {
        name: p.name,
        cur: String(cur),
        next: upImage.value.trim(),
      }),
    }))
  )
    return;
  submitting.value = true;
  try {
    const r = await jobs.setImage(p.cluster, p.ns, p.name, upImage.value.trim(), "savepoint");
    notify(t("jd.upgrade.done", { image: r.image, mode: r.upgradeMode ?? "" }), "ok");
    upOpen.value = false;
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    submitting.value = false;
  }
}

// 创建 Ingress
async function doIngress() {
  if (!ingHost.value.trim()) {
    notify(t("jd.ingress.needHost"), "warn");
    return;
  }
  if (submitting.value) return;
  if (
    !(await confirmDialog({
      message: t("jd.ingress.confirm", { name: p.name, host: ingHost.value.trim() }),
    }))
  )
    return;
  submitting.value = true;
  try {
    const r = await jobs.createIngress(p.cluster, p.ns, p.name, ingHost.value.trim());
    notify(t("jd.ingress.done", { url: r.url ?? "" }), "ok");
    ingOpen.value = false;
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    submitting.value = false;
  }
}

// 自动扩缩容（operator 的 job.autoscaler.* 参数）
const asOpen = ref(false);
const asForm = ref({
  enabled: false,
  utilizationTarget: 0.7,
  metricsWindow: "15m",
  stabilizationInterval: "1h",
  scaleUpMaxFactor: 1.5,
  scaleDownMaxFactor: 0.8,
  vertexMinParallelism: 1,
  vertexMaxParallelism: 0,
});
const asNum = (v: string | undefined, d: number) => (v == null || v === "" ? d : Number(v));

function openAutoscaler() {
  const c = (spec.value.flinkConfiguration ?? {}) as Record<string, string>;
  asForm.value = {
    enabled: c["job.autoscaler.scaling.enabled"] === "true",
    utilizationTarget: asNum(c["job.autoscaler.target.utilization"], 0.7),
    metricsWindow: c["job.autoscaler.metrics.window"] ?? "15m",
    stabilizationInterval: c["job.autoscaler.stabilization.interval"] ?? "1h",
    scaleUpMaxFactor: asNum(c["job.autoscaler.scale-up.max-factor"], 1.5),
    scaleDownMaxFactor: asNum(c["job.autoscaler.scale-down.max-factor"], 0.8),
    vertexMinParallelism: asNum(c["job.autoscaler.vertex.min-parallelism"], 1),
    vertexMaxParallelism: asNum(c["job.autoscaler.vertex.max-parallelism"], 0),
  };
  asOpen.value = true;
}

async function saveAutoscaler() {
  const f = asForm.value;
  try {
    const r = await jobs.setAutoscaler(p.cluster, p.ns, p.name, {
      enabled: f.enabled,
      utilizationTarget: f.utilizationTarget,
      metricsWindow: f.metricsWindow.trim() || undefined,
      stabilizationInterval: f.stabilizationInterval.trim() || undefined,
      scaleUpMaxFactor: f.scaleUpMaxFactor,
      scaleDownMaxFactor: f.scaleDownMaxFactor,
      vertexMinParallelism: f.vertexMinParallelism,
      vertexMaxParallelism: Number.isFinite(f.vertexMaxParallelism)
        ? f.vertexMaxParallelism
        : undefined,
    });
    // operator 不认识的输入由服务端挑出来，这里如实告知，不静默丢弃
    if (r.ignored?.length) notify(r.ignored.join("；"), "warn", 8000);
    notify(
      t("jd.autoscaler.done", {
        state: f.enabled ? t("jd.autoscaler.on") : t("jd.autoscaler.off"),
      }),
      "ok",
    );
    asOpen.value = false;
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function fixExposed() {
  if (
    !(await confirmDialog({
      message: t("jd.nodeport.confirm", { name: p.name }),
    }))
  )
    return;
  try {
    const r = await jobs.setRestExposed(p.cluster, p.ns, p.name, "NodePort");
    notify(t("jd.nodeport.done", { type: r.type ?? "", note: r.note ?? "" }), "ok", 6000);
    await detail.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function runProfile() {
  try {
    const r = await jobs.profile(p.cluster, p.ns, p.name);
    notify(t("jd.flame.done", { body: JSON.stringify(r).slice(0, 120) }), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function openFlamegraph() {
  fgOpen.value = true;
  fgBusy.value = true;
  fgData.value = null;
  try {
    fgData.value = await jobs.flamegraph(p.cluster, p.ns, p.name, "FULL");
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    fgBusy.value = false;
  }
}

const ingressYaml = computed(() => {
  const host = ingHost.value || `${p.name}.flink.example.com`;
  return `apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: ${p.name}-rest
  namespace: ${p.ns}
spec:
  ingressClassName: traefik
  rules:
    - host: ${host}
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: ${p.name}-rest
                port: { number: 8081 }`;
});

function copyIngress() {
  void navigator.clipboard?.writeText(ingressYaml.value);
  notify(t("jd.yaml.copied"), "ok");
}

const bpLevel = computed(() => String((bp.value as LooseJson)?.level ?? "—"));
const bpRatio = computed(() => {
  const v = (bp.value as LooseJson)?.endpoint_id
    ? ((bp.value as LooseJson)?.backpressured as LooseJson | undefined)?.ratio
    : (bp.value as LooseJson)?.ratio;
  return typeof v === "number" ? (v * 100).toFixed(1) + "%" : "—";
});

// 日志尾部行数（REST 日志接口支持 tail，默认 2000）
const logTail = ref(2000);
async function loadLog(kind: "main" | "stdout") {
  logErr.value = "";
  logText.value = t("jd.loading");
  try {
    logText.value = await jobs.log(p.cluster, p.ns, p.name, kind, logTail.value);
  } catch (e) {
    logErr.value = (e as Error).message;
    logText.value = "";
  }
}

// K8s 默认只输出 stdout；Flink 的 /jobmanager/log 需要 log4j file appender，
// 大多数集群拿不到。这里直接读 Pod 日志，避免"看日志"变成死路。
async function loadPodLog(kind: "jm" | "tm" = "jm") {
  logErr.value = "";
  logText.value = t("jd.loading");
  try {
    logText.value = await jobs.podLog(p.cluster, p.ns, p.name, kind, logTail.value);
    await scrollLogToEnd();
  } catch (e) {
    logErr.value = (e as Error).message;
    logText.value = "";
  }
}

// ---- 日志面板：搜索 / 复制 / 下载 / 自动滚底
const logQuery = ref("");
const logEl = ref<HTMLElement | null>(null);
const logView = computed(() => {
  const t = logText.value;
  const q = logQuery.value.trim();
  if (!q) return t;
  return t
    .split("\n")
    .filter((l) => l.toLowerCase().includes(q.toLowerCase()))
    .join("\n");
});
async function scrollLogToEnd() {
  await nextTick();
  const el = logEl.value;
  if (el) el.scrollTop = el.scrollHeight;
}
function copyLog() {
  void navigator.clipboard?.writeText(logText.value);
  notify(t("jd.logs.copied"), "ok");
}
function downloadLog() {
  download(`flink-${p.name}.log`, logText.value);
}
// YAML 里常有 initContainer 的 AK/SK（真实数据里就有），默认打码，避免截图/分享泄露
const yamlRedacted = ref(true);
const yamlDisplay = computed(() => {
  const t = yamlText.value;
  if (!yamlRedacted.value) return t;
  return t
    .replace(
      /((?:ACCESS_KEY|SECRET|PASSWORD|PASSWD|TOKEN|KEY_ID)[A-Z_]*"?\s*:\s*")([^"]{2})[^"]*(")/gi,
      "$1$2****$3",
    )
    .replace(
      /((?:access\.key|secret|password|token)[a-z.]*"\s*:\s*")([^"]{2})[^"]*(")/gi,
      "$1$2****$3",
    );
});

async function loadYaml() {
  yamlText.value = t("jd.loading");
  try {
    yamlText.value = await editor.jobYaml(p.cluster, p.ns, p.name, !yamlRedacted.value);
  } catch (e) {
    yamlText.value = t("jd.yaml.loadFailed", { err: (e as Error).message });
  }
}

// 打码开关必须重新拉一次：只改显示不影响已加载文本，会出现"勾着不勾没区别"。
watch(yamlRedacted, async (redacted) => {
  if (!yamlText.value || yamlText.value.startsWith("//")) return;
  if (
    !redacted &&
    !(await confirmDialog({
      message: t("jd.redactOffConfirm"),
      danger: true,
    }))
  ) {
    yamlRedacted.value = true;
    return;
  }
  await loadYaml();
});
</script>

<template>
  <p
    v-if="loadErr"
    class="mb-2 rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger"
  >
    {{ t("jd.loadFailedLine", { err: String(loadErr) }) }}
  </p>
  <PageHead
    :title="p.name"
    :sub="`${p.cluster} / ${p.ns} · ${s.kind ?? 'FlinkDeployment'}`"
    :badge="String(s.state ?? '')"
    :tone="d?.health === 'green' ? 'ok' : d?.health === 'red' ? 'danger' : 'warn'"
  >
    <template #actions>
      <PageActions :items="headerActions" />
      <a
        :href="`/ui/${p.cluster}/${p.ns}/${p.name}`"
        target="_blank"
        class="rounded-lg bg-surface-3 px-3 py-1.5 text-xs hover:bg-hover"
        >{{ t("jd.openUi") }}</a
      >
      <button
        class="rounded-lg border border-line bg-surface-2 px-2.5 py-1.5 text-xs hover:border-primary/50"
        :title="t('jd.proxyNote')"
        @click="uiOpen = true"
      >
        🛡
      </button>
      <Modal
        v-model="fgOpen"
        :title="t('jd.flameTitle')"
        :sub="`${p.ns} / ${p.name}`"
        width="720px"
      >
        <p v-if="fgBusy" class="text-xs text-fg-2">{{ t("jd.loading.short") }}</p>
        <template v-else-if="fgData">
          <div class="mb-2 flex flex-wrap gap-2 text-2xs text-fg-2">
            <span
              >type: <span class="font-mono">{{ (fgData as LooseJson).type ?? "—" }}</span></span
            >
            <span
              >{{ t("jd.vertex")
              }}<span class="font-mono">{{
                (fgData as LooseJson).endpointId ?? (fgData as LooseJson).vertex ?? "—"
              }}</span></span
            >
          </div>
          <pre
            class="max-h-[380px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-3xs leading-relaxed text-fg-1"
            >{{ JSON.stringify(fgData, null, 2).slice(0, 4000) }}</pre>
          <p class="mt-2 text-2xs text-fg-2">
            {{ t("jd.flameJsonNote")
            }}<a
              :href="`/ui/${p.cluster}/${p.ns}/${p.name}`"
              target="_blank"
              class="text-primary hover:underline"
              >{{ t("jd.flameHowTo") }}</a
            >。
          </p>
        </template>
        <p v-else class="text-xs text-fg-2">{{ t("jd.flameEmpty") }}</p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="fgOpen = false"
          >
            {{ t("common.close") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="openFlamegraph"
          >
            {{ t("jd.resample") }}
          </button>
        </template>
      </Modal>

      <Modal
        v-model="ingOpen"
        :title="t('jd.ingressTitle')"
        :sub="`${p.ns} / ${p.name}-rest`"
        width="640px"
      >
        <label class="flex flex-col gap-1 text-xs">
          <span class="text-fg-2">{{ t("jd.ingressHost") }}</span>
          <input
            v-model="ingHost"
            :placeholder="`${p.name}.flink.example.com`"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          />
        </label>
        <pre
          class="mt-3 max-h-[300px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
          >{{ ingressYaml }}</pre>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("jd.ingressWarn") }} <code class="font-mono">forward-auth</code
          >{{ t("jd.ingressWarnTail") }}
        </p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="ingOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            class="rounded-lg border border-line bg-surface-3 px-3 py-1 text-xs hover:bg-hover"
            @click="copyIngress"
          >
            {{ t("jd.copyYaml") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="doIngress"
          >
            {{ t("jd.createIngress") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="spOpen" :title="t('jd.savepointTitle')" :sub="`${p.ns} / ${p.name}`">
        <label class="flex flex-col gap-1 text-xs">
          <span class="text-fg-2">{{ t("jd.savepointPath") }}</span>
          <input
            v-model="spPath"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
            placeholder="hdfs://ns/path/sp-20260101000000"
          />
        </label>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("jd.savepointNote") }}<code class="font-mono">FlinkStateSnapshot</code
          >{{ t("jd.savepointNoteTail") }}
        </p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="spOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
            @click="doSavepoint"
          >
            {{ t("jd.trigger") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="delOpen" :title="t('jd.deleteTitle')" :sub="`${p.ns} / ${p.name}`" persistent>
        <p class="text-xs text-danger">{{ t("jd.deleteWarn") }}</p>
        <label class="mt-3 flex flex-col gap-1 text-xs">
          <span class="text-fg-2"
            >{{ t("jd.deleteConfirm") }}<code class="font-mono">{{ p.name }}</code></span
          >
          <input
            v-model="delText"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
            :placeholder="p.name"
          />
        </label>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="delOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            :disabled="delText.trim() !== p.name"
            class="rounded-lg bg-danger px-3 py-1 text-xs font-medium text-white disabled:opacity-40"
            @click="confirmDelete"
          >
            {{ t("common.delete") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="uiOpen" :title="t('jd.proxyHowTitle')" width="620px">
        <ul class="ml-4 flex list-disc flex-col gap-2 text-xs leading-relaxed text-fg-1">
          <li>
            {{ t("jd.proxyPath") }}<code class="font-mono">/proxy/{cluster}/{ns}/{name}/</code
            >{{ t("jd.proxyToJm") }}<code class="font-mono">:8081</code>。
          </li>
          <li>
            {{ t("jd.proxyInCluster") }}<code class="font-mono">*-rest.{ns}.svc.cluster.local</code
            >{{ t("jd.proxyOutCluster") }}<code class="font-mono">nodePort</code
            >{{ t("jd.proxyAuto") }}
          </li>
          <li>
            {{ t("jd.proxyReadonly")
            }}<code class="font-mono">cancel / stop / savepoints / jars / rescaling</code
            >{{ t("jd.proxyBlocked") }}<b>403</b>{{ t("jd.proxyAndAudit")
            }}<code class="font-mono">proxy.blocked</code>{{ t("jd.proxyAuditTail") }}
          </li>
          <li>
            {{ t("jd.proxyWs") }}<code class="font-mono">/proxy/…/ws</code>{{ t("jd.proxyWsTail") }}
          </li>
          <li>
            {{ t("jd.proxyAuditEvery") }}<code class="font-mono">ui.open</code
            >{{ t("jd.proxyAuditEveryTail") }}
          </li>
        </ul>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="uiOpen = false"
          >
            {{ t("common.gotIt") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="
              ingOpen = true;
              uiOpen = false;
            "
            :title="t('jd.ingressTitleHint')"
          >
            {{ t("jd.ingressAction") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="upOpen" :title="t('jd.upgradeTitle')" :sub="p.name" width="600px">
        <label class="flex flex-col gap-1 text-xs">
          <span class="text-fg-2">{{ t("jd.newImage") }}</span>
          <input
            v-model="upImage"
            placeholder="harbor/repo/flink:1.19.1"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          />
        </label>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("jd.upgradeSets") }}<code class="font-mono">upgradeMode</code>{{ t("jd.upgradeTo")
          }}<b>savepoint</b>{{ t("jd.upgradeWhy") }}
        </p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="upOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="doUpgrade"
          >
            {{ t("jd.submitUpgrade") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="asOpen" :title="t('jd.autoscalerTitle')" :sub="p.name" width="680px">
        <div
          class="grid gap-3 text-xs"
          style="grid-template-columns: repeat(auto-fit, minmax(200px, 1fr))"
        >
          <label class="flex items-center gap-2"
            ><input v-model="asForm.enabled" type="checkbox" />{{ t("jd.autoscalerOn") }}</label
          >
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerTarget") }}</span>
            <input
              v-model.number="asForm.utilizationTarget"
              type="number"
              step="0.05"
              min="0.1"
              max="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerWindow") }}</span>
            <input
              v-model="asForm.metricsWindow"
              placeholder="15m"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerCooldown") }}</span>
            <input
              v-model="asForm.stabilizationInterval"
              placeholder="1h"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerMaxFactor") }}</span>
            <input
              v-model.number="asForm.scaleUpMaxFactor"
              type="number"
              step="0.5"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerMinFactor") }}</span>
            <input
              v-model.number="asForm.scaleDownMaxFactor"
              type="number"
              step="0.1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerMin") }}</span>
            <input
              v-model.number="asForm.vertexMinParallelism"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.autoscalerMax") }}</span>
            <input
              v-model.number="asForm.vertexMaxParallelism"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
          /></label>
        </div>
        <p class="mt-3 text-2xs text-fg-2">
          {{ t("jd.autoscalerParams") }}<code class="font-mono">job.autoscaler.*</code
          >{{ t("jd.autoscalerStabilise")
          }}{{
            t("jd.slotTotals", {
              p: String(spec.parallelism),
              slots: `${s.taskManagerReplicas} × ${s.slotsPerTm}`,
            })
          }}
        </p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="asOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="saveAutoscaler"
          >
            {{ t("common.save") }}
          </button>
        </template>
      </Modal>

      <Modal v-model="scaleOpen" :title="t('jd.scaleTitle')" :sub="p.name" width="520px">
        <div class="grid grid-cols-2 gap-3 text-xs">
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.parallelism") }}</span>
            <input
              v-model.number="scaleP"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
          <label class="flex flex-col gap-1"
            ><span class="text-fg-2">{{ t("jd.tmReplicas") }}</span>
            <input
              v-model.number="scaleTm"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
          /></label>
        </div>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("jd.slotsTotal") }}{{ spec.taskManagerReplicas }} × {{ s.slotsPerTm
          }}{{ t("jd.slotsTail") }}
        </p>
        <template #footer>
          <button
            class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
            @click="scaleOpen = false"
          >
            {{ t("common.cancel") }}
          </button>
          <button
            class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
            @click="doScale"
          >
            {{ t("common.submit") }}
          </button>
        </template>
      </Modal>
    </template>
  </PageHead>

  <div class="mb-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(160px, 1fr))">
    <Kpi
      :label="t('jd.kpi.parallelism')"
      :value="`${spec.parallelism ?? '—'}${s.parallelismActual && s.parallelismActual !== spec.parallelism ? t('jd.actualSuffix', { n: Number(s.parallelismActual) }) : ''} / ${s.taskManagerReplicas ?? '—'}×${s.slotsPerTm ?? '—'}`"
      :foot="
        s.parallelismActual && s.parallelismActual !== spec.parallelism
          ? t('jd.actualDiffHint')
          : ''
      "
    />
    <Kpi
      :label="t('jd.kpi.throughput')"
      :value="`${fmt((mv.values as LooseJson | undefined)?.numRecordsInPerSecond)} / ${fmt((mv.values as LooseJson | undefined)?.numRecordsOutPerSecond)}`"
      :foot="mv.source === 'flink-rest' ? t('jd.source.live') : t('jd.source.degraded')"
      :tone="mv.degraded ? 'warn' : 'fg'"
    />
    <Kpi :label="t('jd.kpi.checkpoints')" :value="`${ck.completed ?? 0} / ${ck.failed ?? 0}`" />
    <Kpi
      label="restartNonce"
      :value="String(s.restartNonce ?? 0)"
      :foot="`upgradeMode=${spec.upgradeMode ?? '—'}`"
    />
  </div>

  <p
    v-if="mv.degraded"
    class="mb-3 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-xs text-warn"
  >
    {{ t("jd.metricsDegraded") }}{{ mv.degraded }}{{ t("jd.metricsDegradedTail") }}
  </p>

  <Tabs v-model="tab" :tabs="tabs" />

  <template v-if="tab === 'overview'">
    <div class="grid gap-3" style="grid-template-columns: 1.2fr 1fr">
      <Card title="Conditions" :hint="t('jd.healthHint')">
        <div v-if="((s.conditions ?? []) as unknown[]).length" class="flex flex-col">
          <div
            v-for="c in conditions"
            :key="String((c as LooseJson).type ?? '')"
            class="flex items-center gap-3 border-b border-line-2/50 py-1.5 text-xs last:border-0"
          >
            <span class="w-40 font-mono text-fg-1">{{ c.type }}</span>
            <Badge :tone="c.status === 'True' ? 'ok' : 'idle'">{{ c.status }}</Badge>
            <span class="text-fg-2">{{ c.reason }}</span>
          </div>
        </div>
        <div v-else class="text-xs text-fg-2">
          {{
            t("jd.healthDerived", {
              state: String(s.lifecycleState ?? "—"),
              jm: String(s.jobManagerStatus ?? "—"),
            })
          }}
        </div>
      </Card>
      <Card :title="t('jd.lifecycleTitle')" :hint="t('jd.lifecycleHint')">
        <div class="flex flex-wrap gap-2 text-xs">
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="openSavepoint"
          >
            {{ t("jd.savepointTitle") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="act(s.state === 'RUNNING' ? 'stop' : 'start')"
          >
            {{ s.state === "RUNNING" ? t("jd.act.suspendSafer") : t("jd.act.start") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="act('restart')"
          >
            {{ t("jd.restartInPlace") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="openScale"
            :title="t('jd.scaleHint')"
          >
            {{ t("jd.scale") }}
          </button>
          <button
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            :disabled="readonly || acting"
            @click="doRestore()"
          >
            {{ t("jd.restoreFrom") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="
              upOpen = true;
              upImage = String(spec.image ?? '');
            "
          >
            {{ t("jd.upgradeTitle") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="openAutoscaler"
            :title="t('jd.autoscalerHint')"
          >
            {{ t("jd.autoscaler") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="createIngress"
            :title="t('jd.ingressTitleHint')"
          >
            {{ t("jd.createIngressShort") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-danger/40 bg-danger/10 px-2.5 py-1 text-danger hover:bg-danger/20 disabled:opacity-50"
            @click="actCancel"
          >
            {{ t("jd.cancelJob") }}
          </button>
          <button
            :disabled="readonly || acting"
            class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:border-primary/50 disabled:opacity-50"
            @click="doRollback"
          >
            {{ t("jd.rollbackImage") }}
          </button>
          <button
            class="rounded-lg border border-danger/40 bg-danger/10 px-2.5 py-1 text-danger hover:bg-danger/20"
            @click="removeJob"
          >
            {{ t("jd.deleteTitle") }}
          </button>
        </div>
        <p class="mt-2 text-2xs text-fg-2">
          {{ t("jd.restartBumps") }}<code class="font-mono">spec.restartNonce</code
          >{{ t("jd.restartTail") }}<code class="font-mono">spec.job.state</code
          >{{ t("jd.suspendTail") }}
        </p>
      </Card>

      <Card :title="t('jd.basicInfo')">
        <div class="flex flex-col gap-1.5 text-xs">
          <div class="flex justify-between gap-3">
            <span class="text-fg-2">JobID</span
            ><span
              class="cursor-pointer truncate font-mono text-2xs"
              :title="String(s.jobId ?? '')"
              @click="copyText(String(s.jobId ?? ''))"
              >{{ s.jobId ?? t("jd.unassigned") }}</span
            >
          </div>
          <div class="flex justify-between gap-3">
            <span class="text-fg-2">{{ t("jd.image") }}</span><span
              class="cursor-pointer truncate font-mono text-2xs"
              :title="String(spec.image ?? '')"
              @click="copyText(String(spec.image ?? ''))"
              >{{ spec.image ?? "—" }}</span
            >
          </div>
          <div class="flex justify-between gap-3">
            <span class="text-fg-2">Flink</span
            ><span class="font-mono text-2xs">{{ s.flinkVersion ?? "—" }}</span>
          </div>
          <div class="flex justify-between gap-3">
            <span class="text-fg-2">JAR</span
            ><span
              class="cursor-pointer truncate font-mono text-2xs"
              :title="String(spec.jarUri ?? '')"
              @click="copyText(String(spec.jarUri ?? ''))"
              >{{ spec.jarUri ?? "—" }}</span
            >
          </div>
          <div class="flex justify-between gap-3">
            <span class="text-fg-2">{{ t("jd.started") }}</span><span
              class="font-mono text-2xs"
              :title="tsTip(s.startedAt)"
              >{{ s.startedAt ? ts(s.startedAt) : "—" }}</span
            >
          </div>
          <div class="flex items-center justify-between gap-3">
            <span class="text-fg-2">{{ t("jd.restExposed") }}</span>
            <span class="flex items-center gap-2">
              <span
                class="font-mono text-2xs"
                :class="spec.restExposed === 'NodePort' ? 'text-warn' : ''"
                >{{ spec.restExposed ?? "—" }}</span
              >
              <button
                v-if="spec.restExposed && spec.restExposed !== 'NodePort'"
                class="rounded border border-line px-1.5 py-0.5 text-3xs hover:bg-hover"
                :title="t('jd.nodeportHint')"
                @click="fixExposed"
              >
                {{ t("jd.switchNodeport") }}
              </button>
            </span>
          </div>
        </div>
      </Card>
    </div>
  </template>

  <template v-else-if="tab === 'metrics'">
    <div class="mb-3 flex flex-wrap items-center gap-2 text-xs">
      <button
        :disabled="bpBusy"
        class="rounded-lg border border-line bg-surface-3 px-2.5 py-1 hover:bg-hover disabled:opacity-50"
        @click="sampleBackpressure"
      >
        {{ bpBusy ? t("jd.bp.sampling") : t("jd.bp.start") }}
      </button>
      <button
        class="rounded-lg border border-line bg-surface-3 px-2.5 py-1 hover:bg-hover"
        @click="openFlamegraph"
      >
        {{ t("jd.flame") }}
      </button>
      <button
        class="rounded-lg border border-line bg-surface-2 px-2.5 py-1 hover:bg-hover"
        @click="runProfile"
      >
        async-profiler
      </button>
      <span class="text-fg-2">{{ t("jd.cacheNote") }}</span>
    </div>

    <Card
      :title="t('jd.liveMetrics')"
      :hint="t('jd.liveSource', { src: String(mv.source ?? '—'), at: String(mv.sampledAt ?? '') })"
    >
      <div class="grid gap-2" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))">
        <div
          v-for="(v, k) in mv.values ?? {}"
          :key="k"
          class="rounded-lg border border-line-2 px-3 py-2"
        >
          <div class="truncate text-2xs text-fg-2" :title="String(k)">{{ k }}</div>
          <div class="font-mono text-[15px]">{{ fmt(v as number) }}</div>
        </div>
      </div>
      <div v-if="!Object.keys(mv.values ?? {}).length" class="py-6 text-center text-xs text-fg-2">
        {{ t("jd.noLiveData") }}
      </div>
    </Card>

    <Card :title="t('jd.throughputChart')" :hint="t('jd.throughputHint')" class="mt-3">
      <LineChart
        :series="histSeries"
        :labels="histLabels"
        :height="150"
        :y-fmt="(v: number) => String(Math.round(v))"
        :empty="t('jd.throughputEmpty')"
      />
    </Card>

    <Card
      :title="t('jd.operatorTable')"
      :hint="t('jd.vertexCount', { n: vertRows.data?.value?.length ?? 0 })"
      :pad="false"
      class="mt-3"
    >
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">{{ t("jd.col.operator") }}</th>
              <th class="px-3 py-2 text-right">{{ t("jd.parallelism") }}</th>
              <th class="px-3 py-2 text-right">In (rec/s)</th>
              <th class="px-3 py-2 text-right">Out (rec/s)</th>
              <th class="px-3 py-2 text-right">Busy</th>
              <th class="px-3 py-2">{{ t("jd.col.backpressure") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="v in vertRows.data?.value ?? []"
              :key="v.id"
              class="border-b border-line-2/60 last:border-0"
            >
              <td class="max-w-[320px] truncate px-3 py-1.5 font-mono" :title="v.name">
                {{ v.name }}
              </td>
              <td class="px-3 py-1.5 text-right font-mono">{{ v.parallelism }}</td>
              <td class="px-3 py-1.5 text-right font-mono">{{ fmt(v.inRate) }}</td>
              <td class="px-3 py-1.5 text-right font-mono">{{ fmt(v.outRate) }}</td>
              <td class="px-3 py-1.5 text-right font-mono">{{ v.busy.toFixed(0) }}%</td>
              <td class="px-3 py-1.5">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs"
                  :class="
                    v.bp > 500
                      ? 'bg-danger/10 text-danger'
                      : v.bp > 100
                        ? 'bg-warn/10 text-warn'
                        : 'bg-ok/10 text-ok'
                  "
                  >{{ v.bp > 500 ? "HIGH" : v.bp > 100 ? "MED" : "LOW" }}</span
                >
              </td>
            </tr>
            <tr v-if="!(vertRows.data?.value ?? []).length">
              <td colspan="6" class="px-3 py-6 text-center text-xs text-fg-2">
                {{ t("jd.vertexError") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>

    <Card :title="t('jd.backpressureMatrix')" :hint="t('jd.backpressureMatrixHint')" class="mt-3">
      <HeatMap
        :rows="bpMatrix"
        :cols="Math.max(1, bpHist.length)"
        :time-hint="{ start: t('jd.timeEarlier'), end: t('jd.timeNow') }"
      />
      <p v-if="!bpMatrix.length" class="py-3 text-center text-2xs text-fg-2">
        {{ t("jd.backpressureEmpty") }}
      </p>
    </Card>

    <Card
      :title="t('jd.col.backpressure')"
      :hint="bp ? t('jd.bp.sampled') : t('jd.bp.notSampled')"
      class="mt-3"
    >
      <div
        v-if="bp"
        class="grid gap-3"
        style="grid-template-columns: repeat(auto-fit, minmax(160px, 1fr))"
      >
        <div class="rounded-lg border border-line-2 px-3 py-2">
          <div class="text-2xs text-fg-2">{{ t("jd.col.level") }}</div>
          <div
            class="font-mono text-[15px]"
            :class="
              bpLevel === 'HIGH' ? 'text-danger' : bpLevel === 'LOW' ? 'text-ok' : 'text-warn'
            "
          >
            {{ bpLevel }}
          </div>
        </div>
        <div class="rounded-lg border border-line-2 px-3 py-2">
          <div class="text-2xs text-fg-2">{{ t("jd.col.ratio") }}</div>
          <div class="font-mono text-[15px]">{{ bpRatio }}</div>
        </div>
        <div class="rounded-lg border border-line-2 px-3 py-2">
          <div class="text-2xs text-fg-2">vertex</div>
          <div class="truncate font-mono text-2xs">{{ (bp as LooseJson).vertex ?? "—" }}</div>
        </div>
      </div>
      <p v-else class="text-xs text-fg-2">
        {{ t("jd.sampleHint") }}
      </p>
      <p class="mt-2 text-2xs text-fg-2">
        {{ t("jd.timeRangeLabel") }} {{ timeRange }} {{ t("jd.timeRangeHint") }}
      </p>
    </Card>
  </template>

  <template v-else-if="tab === 'ckpt'">
    <Card :title="t('jd.checkpointCard')" :hint="t('jd.checkpointHint')">
      <div class="grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(160px, 1fr))">
        <Kpi :label="t('jd.cp.completed')" :value="ckView.completed" tone="ok" />
        <Kpi
          :label="t('jd.cp.failed')"
          :value="ckView.failed"
          :tone="ck.failed ? 'danger' : 'fg'"
        />
        <Kpi :label="t('jd.cp.restored')" :value="ckView.restored" />
        <Kpi :label="t('jd.cp.duration')" :value="ckView.lastMs ? `${ckView.lastMs} ms` : '—'" />
      </div>
      <div v-if="ckView.hasLast" class="mt-3 text-xs text-fg-1">
        ID <span class="font-mono">{{ ckView.lastId }}</span> {{ t("jd.size") }}
        <span class="font-mono">{{ ckView.lastSize }}</span> {{ t("jd.time") }}
        <span class="font-mono">{{ ts(ckView.lastAt) }}</span>
      </div>
    </Card>
  </template>

  <template v-else-if="tab === 'snapshots'">
    <Card title="Savepoint" :hint="t('jd.spHistoryCount', { n: spView.historyLen })">
      <div v-if="spView.hasLast" class="mb-3 text-xs">
        {{ t("jd.latest") }}<span class="font-mono">{{ spView.lastPath }}</span>
      </div>
      <div
        v-for="(h, i) in spView.history"
        :key="i"
        class="flex items-center gap-2 border-b border-line-2/50 py-1.5 text-2xs last:border-0"
      >
        <span class="font-mono text-fg-1">#{{ h.id }}</span>
        <span class="min-w-0 flex-1 truncate text-fg-2">{{ h.path ?? "—" }}</span>
        <span class="font-mono text-3xs text-fg-2">{{ ts(h.timestamp) }}</span>
        <button
          v-if="h.path"
          class="rounded border border-line px-1.5 py-0.5 text-3xs hover:border-primary/50"
          @click="doRestore(h.path)"
        >
          {{ t("jd.restoreFromThis") }}
        </button>
      </div>
      <div v-if="!spView.history.length" class="py-2 text-center text-xs text-fg-2">
        {{ t("jd.noSavepointHistory") }}
      </div>
    </Card>

    <Card :title="t('jd.snapshotCard')" :hint="t('jd.snapCount', { n: snapRows.length })">
      <div v-if="!snapRows.length" class="py-3 text-center text-xs text-fg-2">
        {{ t("jd.snapshotEmpty") }}
      </div>
      <div
        v-for="s in snapRows"
        :key="s.name"
        class="flex items-center gap-2 border-b border-line-2/50 py-1.5 text-2xs last:border-0"
      >
        <Badge
          :tone="
            s.state === 'COMPLETED'
              ? 'ok'
              : s.state === 'FAILED' || s.state === 'ABANDONED'
                ? 'danger'
                : 'warn'
          "
          >{{ s.state }}</Badge
        >
        <span class="min-w-0 flex-1 truncate font-mono text-3xs text-fg-2">{{
          s.path ?? s.name
        }}</span>
        <button
          v-if="s.path && s.state === 'COMPLETED'"
          class="rounded border border-line px-1.5 py-0.5 text-3xs hover:border-primary/50"
          @click="doRestore(s.path)"
        >
          {{ t("jd.restoreFromThis") }}
        </button>
      </div>
    </Card>
  </template>

  <template v-else-if="tab === 'exceptions'">
    <Card :title="t('jd.exceptionsCard')" :hint="t('jd.exceptionsHint')">
      <p v-if="exceptions.isLoading.value" class="text-xs text-fg-2">{{ t("jd.loading") }}</p>
      <p
        v-else-if="exceptions.error.value"
        class="rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-2xs text-warn"
      >
        {{ (exceptions.error.value as Error).message }}
      </p>
      <template v-else-if="exceptions.data.value">
        <pre
          class="max-h-[260px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-danger"
          >{{ exceptions.data.value.rootException ?? t("jd.noRootException") }}</pre>
        <div
          v-for="(x, i) in exceptions.data.value.exceptions ?? []"
          :key="i"
          class="mt-2 border-t border-line-2/50 pt-2"
        >
          <div class="text-2xs text-fg-1">
            {{ x.exceptionName ?? t("jd.detail") }}
            <span class="text-fg-2">{{ x.taskName ?? "" }}</span>
          </div>
          <pre class="mt-1 max-h-[200px] overflow-auto font-mono text-3xs text-fg-2">{{
            x.stacktrace ?? ""
          }}</pre>
        </div>
      </template>
    </Card>
  </template>

  <template v-else-if="tab === 'logs'">
    <Card :title="t('jd.logsCard')" :hint="t('jd.logsHint')">
      <template #actions>
        <select
          v-model.number="logTail"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1 text-2xs"
          :title="t('jd.logsTail')"
        >
          <option :value="500">{{ t("jd.logs500") }}</option>
          <option :value="2000">{{ t("jd.logs2000") }}</option>
          <option :value="5000">{{ t("jd.logs5000") }}</option>
          <option :value="20000">{{ t("jd.logs20000") }}</option>
        </select>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="loadLog('main')"
        >
          {{ t("jd.loadLogFile") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="loadLog('stdout')"
        >
          {{ t("jd.loadStdout") }}
        </button>
        <button
          class="rounded-lg bg-primary/10 px-2.5 py-1 text-2xs text-primary hover:bg-primary/20"
          @click="loadPodLog('jm')"
        >
          {{ t("jd.podLogsJm") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="loadPodLog('tm')"
        >
          {{ t("jd.podLogsTm") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="copyLog"
        >
          {{ t("jd.copy") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="downloadLog"
        >
          {{ t("jd.download") }}
        </button>
      </template>
      <div class="mb-2 flex items-center gap-2">
        <input
          v-model="logQuery"
          :placeholder="t('jd.logFilter')"
          class="w-72 rounded-lg border border-line bg-surface-2 px-2 py-1 text-2xs font-mono"
        />
        <span class="text-2xs text-fg-2">{{
          logQuery
            ? t("jd.logMatched", { n: logView ? logView.split("\n").length : 0 })
            : t("jd.logLines", { n: logText ? logText.split("\n").length : 0 })
        }}</span>
      </div>
      <p
        v-if="logErr"
        class="mb-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-2xs text-warn"
      >
        {{ logErr }}
      </p>
      <pre
        ref="logEl"
        class="max-h-[480px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
        >{{ logView || (logQuery ? t("jd.logNoMatch") : t("jd.logLoadHint")) }}</pre>
    </Card>
  </template>

  <template v-else-if="tab === 'config'">
    <Card title="flinkConfiguration" :hint="t('jd.configCount', { n: cfg.length })">
      <div class="max-h-[520px] overflow-auto">
        <div
          v-for="[k, v] in cfg"
          :key="k"
          class="flex gap-3 border-b border-line-2/50 py-1.5 text-2xs last:border-0"
        >
          <span class="w-[46%] shrink-0 truncate font-mono text-fg-1" :title="k">{{ k }}</span>
          <span class="truncate font-mono text-fg-2" :title="String(v)">{{ v }}</span>
        </div>
        <div v-if="!cfg.length" class="py-4 text-center text-fg-2">{{ t("jd.noExtraConfig") }}</div>
      </div>
    </Card>
  </template>

  <template v-else-if="tab === 'yaml'">
    <Card :title="t('jd.crdYaml')" :hint="t('jd.crdYamlHint')">
      <template #actions>
        <label class="flex cursor-pointer items-center gap-1 text-2xs text-fg-2">
          <input v-model="yamlRedacted" type="checkbox" class="cursor-pointer" />
          {{ t("jd.redactOn") }}
        </label>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="loadYaml"
        >
          {{ t("jd.loadYaml") }}
        </button>
      </template>
      <pre
        class="max-h-[560px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
        >{{ yamlDisplay || t("jd.loadHint") }}</pre>
    </Card>
  </template>

  <template v-else>
    <Card
      :title="t('jd.eventsCard')"
      :hint="t('jd.eventCount', { n: events.data.value?.items.length ?? 0 })"
    >
      <p v-if="events.isLoading.value" class="text-xs text-fg-2">{{ t("jd.loading") }}</p>
      <div v-else-if="events.data.value?.items.length" class="flex flex-col">
        <div
          v-for="(e, i) in events.data.value.items"
          :key="i"
          class="flex gap-3 border-b border-line-2/50 py-1.5 text-2xs last:border-0"
        >
          <span class="w-16 shrink-0" :class="e.type === 'Warning' ? 'text-warn' : 'text-fg-2'">{{
            e.type || "Normal"
          }}</span>
          <span class="w-36 shrink-0 truncate font-mono text-fg-1">{{ e.reason }}</span>
          <span class="min-w-0 flex-1 text-fg-1">{{ e.message }}</span>
          <span class="shrink-0 font-mono text-3xs text-fg-2">×{{ e.count }}</span>
          <span class="w-40 shrink-0 text-right font-mono text-3xs text-fg-2">{{
            fmtLocal(e.lastTimestamp)
          }}</span>
        </div>
      </div>
      <div v-else class="py-4 text-center text-xs text-fg-2">
        {{ t("jd.noEvents") }}
      </div>
    </Card>
  </template>
</template>
