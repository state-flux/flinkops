/**
 * API 客户端：与 axum 后端的唯一出口。
 *
 * 约定：
 * - 认证用 HttpOnly cookie（`flinkops_session`），所以 fetch 必须带 `credentials: "same-origin"`；
 * - 错误统一成 `ApiError`，页面直接读 `code` / `message`；
 * - 实时指标是"毫秒级、会失败、会降级"的，所以返回值里带 `degraded`，UI 要显式呈现；
 * - 类型来自 ts-rs 从 Rust DTO 生成（见 apps/web/README.md），不要手写第二份。
 */
export type ApiErrorBody = { error: { code: string; message: string } };

export class ApiError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

/** 401 时的全局回调（由 main.ts 注册为"跳登录页"）。 */
let unauthorizedHandler: (() => void) | null = null;

export function onUnauthorized(fn: () => void) {
  unauthorizedHandler = fn;
}

type Me = { user: string; readonly: boolean; clusters: string[]; prometheus: boolean };

/** 会话探测缓存：路由守卫会频繁调用，不能每次都打服务端。 */
let sessionProbe: Promise<Me> | null = null;

export function ensureSession(): Promise<Me> {
  sessionProbe ??= request<Me>("/api/me").catch((e) => {
    sessionProbe = null;
    throw e;
  });
  return sessionProbe;
}

export function resetSession() {
  sessionProbe = null;
}

/// 平台只读模式下，前端在发请求前就拦住写操作（后端仍会 403，但没必要等一个来回）
let readonlyMode = false;
export function setReadonlyMode(v: boolean) {
  readonlyMode = v;
}

/** 从 Cookie 里取 CSRF 令牌（登录时由服务端下发，非 HttpOnly）。 */
function csrfToken(): string {
  if (typeof document === "undefined") return "";
  const hit = document.cookie
    .split(";")
    .map((s) => s.trim())
    .find((s) => s.startsWith("flinkops_csrf="));
  return hit ? decodeURIComponent(hit.slice("flinkops_csrf=".length)) : "";
}

/**
 * 全局 fetch 兜底：任何 `/api/*` 的写请求自动带上 CSRF 头。
 *
 * 背景：页面里还有一些直接 `fetch()` 的地方（SQL 工作台等 9 处），
 * 它们绕过了 request()；CSRF 上线后这些请求会被 403。与其逐个改，
 * 不如在入口处统一补一次 —— 以后新写 raw fetch 也不会踩坑。
 */
const rawFetch = window.fetch.bind(window);
window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
  const method = (init?.method ?? "GET").toUpperCase();
  const url = typeof input === "string" ? input : input instanceof URL ? input.pathname : input.url;
  const needsCsrf =
    method !== "GET" &&
    method !== "HEAD" &&
    url.startsWith("/api/") &&
    !url.startsWith("/api/login");
  if (!needsCsrf) return rawFetch(input, init);
  const headers = new Headers(init?.headers ?? {});
  if (!headers.has("x-csrf-token")) headers.set("x-csrf-token", csrfToken());
  return rawFetch(input, { ...init, headers });
};

export async function request<T>(
  path: string,
  init: RequestInit & { timeoutMs?: number } = {},
): Promise<T> {
  const method = (init.method ?? "GET").toUpperCase();
  if (readonlyMode && method !== "GET" && method !== "HEAD") {
    throw new ApiError("readonly", "平台处于只读模式（由设置页开启），写操作已禁用", 403);
  }
  // 没有超时的 fetch 会让按钮永久 disabled（后端/反代挂住时），
  // 也可能让 30s 轮询不断叠加请求。15s 是这里的经验值。
  // 个别接口（从 savepoint 恢复要挂起→启动→等新 jobId）天然超过 15s，
  // 由调用方给 timeoutMs 放宽，其余一律 15s 兜底。
  const signal = init.signal ?? AbortSignal.timeout(init.timeoutMs ?? 15_000);
  let resp: Response;
  try {
    resp = await fetch(path, {
      credentials: "same-origin",
      headers: {
        "content-type": "application/json",
        // 写操作必须带 CSRF 双提交令牌（登录时下发的 flinkops_csrf）
        ...(method === "GET" || method === "HEAD" ? {} : { "x-csrf-token": csrfToken() }),
        ...(init.headers ?? {}),
      },
      ...init,
      signal,
    });
  } catch (e) {
    const aborted = (e as Error)?.name === "TimeoutError" || (e as Error)?.name === "AbortError";
    throw new ApiError(
      aborted ? "timeout" : "network_error",
      aborted ? "请求超时（15s）：后端或集群没有响应" : `网络错误：${(e as Error).message}`,
      0,
    );
  }
  if (!resp.ok) {
    // 登录接口自己的 401 是"密码错"，不能触发跳转
    if (resp.status === 401 && !path.startsWith("/api/login")) {
      unauthorizedHandler?.();
    }
    let code = "http_error";
    let message = `HTTP ${resp.status}`;
    try {
      const body = (await resp.json()) as ApiErrorBody;
      code = body.error?.code ?? code;
      message = body.error?.message ?? message;
    } catch {
      /* 非 JSON 响应（例如反代的纯文本 502） */
    }
    throw new ApiError(code, message, resp.status);
  }
  if (resp.status === 204) return undefined as T;
  const ct = resp.headers.get("content-type") ?? "";
  return (ct.includes("application/json") ? resp.json() : resp.text()) as Promise<T>;
}

// ---------------------------------------------------------------- 认证

export const auth = {
  login: async (username: string, password: string, totp?: string) => {
    const r = await request<{ user: string; csrf?: string }>("/api/login", {
      method: "POST",
      body: JSON.stringify({ username, password, totp: totp || undefined }),
    });
    resetSession();
    return r;
  },
  logout: async () => {
    const r = await request<{ ok: boolean }>("/api/logout", { method: "POST" });
    resetSession();
    return r;
  },
  me: () =>
    request<{
      user: string;
      readonly: boolean;
      clusters: string[];
      prometheus: boolean;
      role?: string;
      allowNs?: string[];
      totpEnabled?: boolean;
      cookieSecure?: boolean | null;
    }>("/api/me"),
  /** 自助改密码（本地账号） */
  changePassword: (oldPassword: string, newPassword: string) =>
    request<{ ok: boolean }>("/api/me/password", {
      method: "POST",
      body: JSON.stringify({ oldPassword, newPassword }),
    }),
  totpSetup: () =>
    request<{ ok: boolean; secret: string; otpauth: string; note: string }>("/api/me/2fa/setup", {
      method: "POST",
    }),
  totpEnable: (code: string) =>
    request<{ ok: boolean; totpEnabled: boolean }>("/api/me/2fa/enable", {
      method: "POST",
      body: JSON.stringify({ code }),
    }),
  totpDisable: (code: string) =>
    request<{ ok: boolean; totpEnabled: boolean }>("/api/me/2fa/disable", {
      method: "POST",
      body: JSON.stringify({ code }),
    }),
};

// ---------------------------------------------------------------- 作业

export type Condition = {
  type: string;
  status: string;
  reason?: string;
  message?: string;
  lastTransitionTime?: string;
};
export type JobSummary = {
  jobManagerCpu?: string;
  jobManagerMemory?: string;
  taskManagerCpu?: string;
  taskManagerMemory?: string;
  cluster: string;
  namespace: string;
  name: string;
  kind: string;
  state: string;
  jobId?: string;
  parallelism?: number;
  /** job 图实际并行度（spec 只写入口算子，6 TM × 4 slots 这类要靠它看） */
  parallelismActual?: number;
  taskManagerReplicas?: number;
  slotsPerTm?: number;
  restartNonce?: number;
  upgradeMode?: string;
  image?: string;
  flinkVersion?: string;
  restExposed?: string;
  jobManagerStatus?: string;
  reconciliationState?: string;
  error?: string;
  generation?: number;
  observedGeneration?: number;
  conditions: Condition[];
  updatedAt?: string;
  /** jobStatus.startTime（RFC3339），列表页算运行时长用 */
  startedAt?: string;
  /** 后端算好的健康度：green / yellow / red */
  health?: "green" | "yellow" | "red";
};

export const jobs = {
  list: (
    params: { cluster?: string; namespace?: string; refresh?: boolean; timeoutMs?: number } = {},
  ) => {
    const q = new URLSearchParams();
    if (params.cluster) q.set("cluster", params.cluster);
    if (params.namespace) q.set("namespace", params.namespace);
    if (params.refresh) q.set("refresh", "true");
    return request<{
      items: JobSummary[];
      count: number;
      total?: number;
      asOf?: string;
      indexAgeSecs?: number;
    }>(
      `/api/jobs?${q}`,
      // 全量同步要逐集群 list 一遍，大集群 15s 不够；只在 refresh 时放宽
      { timeoutMs: params.timeoutMs ?? (params.refresh ? 120_000 : undefined) },
    );
  },
  detail: (cluster: string, ns: string, name: string) =>
    request<Record<string, unknown>>(`/api/jobs/${cluster}/${ns}/${name}`),
  /** 提交作业：dryRun=true 只校验不落库 */
  create: (yaml: string, opts: { cluster?: string; dryRun?: boolean } = {}) => {
    const q = new URLSearchParams();
    if (opts.cluster) q.set("cluster", opts.cluster);
    if (opts.dryRun) q.set("dryRun", "true");
    return request<{
      ok: boolean;
      name: string;
      namespace: string;
      cluster: string;
      dryRun: boolean;
    }>(`/api/jobs?${q}`, { method: "POST", body: JSON.stringify({ yaml }) });
  },
  /** K8s 事件（服务端 fieldSelector 过滤） */
  events: (cluster: string, ns: string, name: string) =>
    request<{ items: K8sEvent[]; count: number }>(`/api/jobs/${cluster}/${ns}/${name}/events`),
  /** Flink REST 异常（根因堆栈） */
  exceptions: (cluster: string, ns: string, name: string) =>
    request<FlinkExceptions>(`/api/jobs/${cluster}/${ns}/${name}/exceptions`),
  /** 删除作业（不可逆） */
  remove: (cluster: string, ns: string, name: string) =>
    request<{ ok: boolean; deleted: string }>(`/api/jobs/${cluster}/${ns}/${name}`, {
      method: "DELETE",
    }),
  /** 滚动升级到新镜像 */
  setImage: (cluster: string, ns: string, name: string, image: string, upgradeMode = "savepoint") =>
    request<{ ok: boolean; image: string; upgradeMode: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/image`,
      {
        method: "POST",
        body: JSON.stringify({ image, upgradeMode }),
      },
    ),
  /** 创建工作 Ingress（域名直连） */
  createIngress: (cluster: string, ns: string, name: string, host: string, className?: string) =>
    request<{ ok: boolean; ingress: string; host: string; url: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/ingress`,
      {
        method: "POST",
        body: JSON.stringify({ host, className }),
      },
    ),
  /** 从 savepoint 恢复 */
  // 恢复 = 挂起 → 写路径 → 启动，最坏要几分钟；单独放宽超时（默认 15s 会提前 abort）
  restore: (cluster: string, ns: string, name: string, path?: string) =>
    request<{ ok: boolean; from?: string; confirmed?: boolean; phases?: string[]; note?: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/restore`,
      { method: "POST", body: JSON.stringify({ path }), timeoutMs: 240_000 },
    ),
  cancel: (cluster: string, ns: string, name: string) =>
    request<{ ok: boolean; state: string; upgradeMode: string; note?: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/cancel`,
      { method: "POST" },
    ),
  rollback: (cluster: string, ns: string, name: string, image?: string) =>
    request<{ ok: boolean; from: string; to: string; note?: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/rollback`,
      { method: "POST", body: JSON.stringify({ image }) },
    ),
  /** 扩容：并行度 / TM 副本 */
  scale: (
    cluster: string,
    ns: string,
    name: string,
    body: { parallelism?: number; taskManagerReplicas?: number },
  ) =>
    request<{ ok: boolean }>(`/api/jobs/${cluster}/${ns}/${name}/scale`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /** 切换 REST Service 暴露方式（ClusterIP 在集群外连不上时用） */
  setRestExposed: (cluster: string, ns: string, name: string, kind: string) =>
    request<{ ok: boolean; type: string; note: string }>(
      `/api/jobs/${cluster}/${ns}/${name}/rest-exposed`,
      {
        method: "POST",
        body: JSON.stringify({ kind }),
      },
    ),
  /** 开关 autoscaler */
  setAutoscaler: (
    cluster: string,
    ns: string,
    name: string,
    cfg: {
      enabled: boolean;
      utilizationTarget?: number;
      metricsWindow?: string;
      stabilizationInterval?: string;
      scaleUpMaxFactor?: number;
      scaleDownMaxFactor?: number;
      vertexMinParallelism?: number;
      vertexMaxParallelism?: number;
    },
  ) =>
    request<{ ok: boolean; enabled: boolean; ignored?: string[] }>(
      `/api/jobs/${cluster}/${ns}/${name}/autoscaler`,
      {
        method: "POST",
        body: JSON.stringify(cfg),
      },
    ),
  /** 顶点背压（Flink 会采样 100 次任务线程，30–60s 一次即可） */
  backpressure: (cluster: string, ns: string, name: string, vertex?: string) => {
    const q = vertex ? `?vertex=${encodeURIComponent(vertex)}` : "";
    return request<Record<string, unknown>>(`/api/jobs/${cluster}/${ns}/${name}/backpressure${q}`);
  },
  /** 顶点火焰图（Traces JSON，不是 SVG） */
  flamegraph: (cluster: string, ns: string, name: string, kind = "FULL", vertex?: string) => {
    const q = new URLSearchParams({ type: kind });
    if (vertex) q.set("vertex", vertex);
    return request<Record<string, unknown>>(`/api/jobs/${cluster}/${ns}/${name}/flamegraph?${q}`);
  },
  /** 触发 async-profiler（Flink 1.18+） */
  profile: (cluster: string, ns: string, name: string) =>
    request<Record<string, unknown>>(`/api/jobs/${cluster}/${ns}/${name}/profile`, {
      method: "POST",
    }),
  /** 实时指标：走 Flink REST，若失败返回 degraded 原因（不要静默成 0） */
  liveMetrics: (cluster: string, ns: string, name: string, ids?: string[]) => {
    const q = ids?.length ? `?ids=${encodeURIComponent(ids.join(","))}` : "";
    return request<{
      jobId: string;
      values: Record<string, number>;
      source: "flink-rest" | "prometheus" | "none";
      sampledAt: string;
      degraded?: string;
    }>(`/api/jobs/${cluster}/${ns}/${name}/metrics${q}`);
  },
  log: (cluster: string, ns: string, name: string, kind: "main" | "stdout" = "main", tail = 2000) =>
    request<string>(`/api/jobs/${cluster}/${ns}/${name}/log?kind=${kind}&tail=${tail}`),
  setState: (cluster: string, ns: string, name: string, running: boolean) =>
    request<{ ok: boolean }>(`/api/jobs/${cluster}/${ns}/${name}/state`, {
      method: "POST",
      body: JSON.stringify({ running }),
    }),
  restart: (cluster: string, ns: string, name: string) =>
    request<{ ok: boolean; restartNonce: number }>(`/api/jobs/${cluster}/${ns}/${name}/restart`, {
      method: "POST",
    }),
  savepoint: (cluster: string, ns: string, name: string, path?: string) =>
    request<{
      ok: boolean;
      /** cr = 建了 FlinkStateSnapshot；rest = CR 被 webhook 拒绝、降级打 JM REST */
      mode?: "cr" | "rest";
      snapshot?: string | null;
      /** FlinkStateSnapshot 的 status.state（IN_PROGRESS / COMPLETED / FAILED / ABANDONED） */
      state?: string;
      /** true 表示降级：快照不在 CR/快照列表里，恢复下拉选不到 */
      degraded?: boolean;
      requestId?: string;
      note?: string;
    }>(`/api/jobs/${cluster}/${ns}/${name}/savepoint`, {
      method: "POST",
      body: JSON.stringify({ path: path || undefined }),
    }),
  /** 保存编辑后的 CR（服务端做 resourceVersion CAS） */
  update: (cluster: string, ns: string, name: string, yaml: string) =>
    request<{ ok: boolean; changed?: number; paths?: string[] }>(
      `/api/jobs/${cluster}/${ns}/${name}`,
      { method: "PUT", body: JSON.stringify({ yaml }) },
    ),
  /** 直接读 Pod 日志（REST 文件日志拿不到时的兜底） */
  podLog: (cluster: string, ns: string, name: string, kind: "jm" | "tm" = "jm", tail = 2000) =>
    request<string>(`/api/jobs/${cluster}/${ns}/${name}/pod-log?kind=${kind}&tail=${tail}`),
};

// ---------------------------------------------------------------- 其它

export const profiles = {
  list: () => request<{ items: unknown[] }>("/api/profiles"),
  save: (id: string, payload: unknown) =>
    request<{ ok: boolean }>(`/api/profiles/${id}`, {
      method: "PUT",
      body: JSON.stringify(payload),
    }),
};

export const audit = {
  /** CSV 直链（浏览器下载，带 Cookie；CSRF 只对写方法生效） */
  csvUrl: (
    params: { action?: string; actor?: string; result?: string; q?: string; since?: string } = {},
  ) => {
    const q = new URLSearchParams({ format: "csv" });
    for (const [k, v] of Object.entries(params)) if (v) q.set(k, String(v));
    return `/api/audit?${q}`;
  },
  list: (
    params: {
      action?: string;
      actor?: string;
      result?: string;
      q?: string;
      since?: string;
      limit?: number;
      offset?: number;
    } = {},
  ) => {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== "") q.set(k, String(v));
    }
    return request<{ items: AuditEntry[]; count: number; total: number }>(`/api/audit?${q}`);
  },
};

export type AuditEntry = {
  id: string;
  ts: string;
  actor: string;
  action: string;
  target: string;
  cluster: string;
  namespace: string;
  result: "ok" | "denied" | "error";
  detail?: string;
};

export const sessions = {
  list: (cluster?: string) =>
    request<{ items: SessionCluster[] }>(`/api/sessions${cluster ? `?cluster=${cluster}` : ""}`),
  /** 创建 session 模式的 FlinkDeployment；dryRun=true 只校验不落库 */
  create: (body: {
    name: string;
    cluster?: string;
    namespace?: string;
    profileId?: string;
    slots?: number;
    taskManagerReplicas?: number;
    flinkVersion?: string;
    image?: string;
    dryRun?: boolean;
  }) =>
    request<{ ok: boolean; cluster: string; namespace: string; name: string; dryRun: boolean }>(
      "/api/sessions",
      { method: "POST", body: JSON.stringify(body) },
    ),
};

export type SessionCluster = {
  cluster: string;
  namespace: string;
  name: string;
  state: string;
  flinkVersion?: string;
  image?: string;
  jobManagerCpu?: string;
  jobManagerMemory?: string;
  taskManagerReplicas: number;
  slotsPerTm: number;
  usedSlots?: number;
  idleTtl?: string;
  sqlGateway?: string;
};

/** 原生 Flink UI 的只读反代地址（iframe / 新标签打开） */
export function flinkUiUrl(cluster: string, ns: string, name: string) {
  return `/proxy/${cluster}/${ns}/${name}/ui/`;
}

// ---------------------------------------------------------------- 编辑器

export type Finding = {
  severity: "error" | "warn" | "info";
  code: string;
  path: string;
  message: string;
};

export type SpecDiff = { path: string; before?: string; after?: string };

export const editor = {
  /** 读取集群里该作业的 CRD（YAML 文本） */
  jobYaml: (cluster: string, ns: string, name: string, reveal = false) =>
    request<string>(`/api/jobs/${cluster}/${ns}/${name}/yaml?redact=${reveal ? "false" : "true"}`),
  /** 服务端解析 + 校验，返回 spec 供 diff */
  validate: (yaml: string) =>
    request<{ findings: Finding[]; spec: unknown; hasErrors: boolean }>("/api/validate", {
      method: "POST",
      body: JSON.stringify({ yaml }),
    }),
  /** 纯排版：解析 + 重新序列化，不注入基线、不剥外壳 */
  format: (yaml: string) =>
    request<{ yaml: string }>("/api/format", {
      method: "POST",
      body: JSON.stringify({ yaml }),
    }),
  diff: (before: unknown, after: unknown) =>
    request<{ diffs: SpecDiff[]; count: number }>("/api/diff", {
      method: "POST",
      body: JSON.stringify({ before, after }),
    }),
  /** 用基线 + 覆盖项渲染 spec */
  render: (profileId: string | undefined, overrides: unknown) =>
    request<{ spec: unknown; yaml: string; findings: Finding[] }>("/api/render", {
      method: "POST",
      body: JSON.stringify({ profileId, overrides }),
    }),
};
export type K8sEvent = {
  type: string;
  reason: string;
  message: string;
  count: number;
  firstTimestamp?: string;
  lastTimestamp?: string;
  source?: string;
};

export type FlinkExceptions = {
  rootException?: string;
  truncated?: boolean;
  exceptions?: { exceptionName?: string; stacktrace?: string; taskName?: string }[];
};

export type SnapshotItem = {
  cluster: string;
  namespace: string;
  name: string;
  jobName?: string;
  state: string;
  path?: string;
  createdAt?: string;
};

export const snapshots = {
  list: (params: { cluster?: string; namespace?: string; state?: string } = {}) => {
    const q = new URLSearchParams();
    if (params.cluster) q.set("cluster", params.cluster);
    if (params.namespace) q.set("namespace", params.namespace);
    if (params.state) q.set("state", params.state);
    return request<{ items: SnapshotItem[]; count: number; unsupportedClusters?: string[] }>(
      `/api/snapshots?${q}`,
    );
  },
};
// ---------------------------------------------------------------- Prometheus

export type PromSample = {
  metric: Record<string, string>;
  value?: [number, string];
  values?: [number, string][];
};
export type PromResult = {
  status: string;
  data?: { resultType: string; result: PromSample[] };
  error?: string;
};

export const prom = {
  query: (expr: string) =>
    request<PromResult>(`/api/metrics/query?query=${encodeURIComponent(expr)}`),
  range: (expr: string, opts: { start?: number; end?: number; step?: number } = {}) => {
    const q = new URLSearchParams({ query: expr });
    if (opts.start) q.set("start", String(opts.start));
    if (opts.end) q.set("end", String(opts.end));
    if (opts.step) q.set("step", String(opts.step));
    return request<PromResult>(`/api/metrics/query_range?${q}`);
  },
  discover: () =>
    request<{
      configured: string | null;
      count: number;
      items: {
        cluster: string;
        name: string;
        namespace: string;
        nodePort: number | null;
        baseUrl: string | null;
      }[];
    }>("/api/prometheus/discover"),
};
// ---------------------------------------------------------------- 集群能力

export type OperatorInfo = {
  namespace: string;
  name: string;
  images: string[];
  readyReplicas: number;
};
export type ClusterCapability = {
  id: string;
  inCluster: boolean;
  supportedKinds: string[];
  kindCount: number;
  operators: OperatorInfo[];
};
export type ClusterInfo = {
  clusters: ClusterCapability[];
  jobs: { total: number; withConditions: number; withoutObservedGeneration: number; note: string };
};

export const system = {
  clusterInfo: () => request<ClusterInfo>("/api/cluster-info"),
};
export const snapshotsApi = {
  remove: (cluster: string, ns: string, name: string) =>
    request<{ ok: boolean; deleted: string }>(`/api/snapshots/${cluster}/${ns}/${name}`, {
      method: "DELETE",
    }),
};

export const admin = {
  vacuum: () =>
    request<{
      ok: boolean;
      vacuumed: boolean;
      beforeBytes?: number;
      afterBytes?: number;
      note: string;
    }>("/api/admin/vacuum", { method: "POST" }),
  backup: () =>
    request<{ kind: string; path?: string; sizeBytes?: number; hint: string }>("/api/admin/backup"),
  diagnostic: () => request<Record<string, unknown>>("/api/admin/diagnostic"),
  getSettings: () =>
    request<{
      effective: Record<string, string>;
      saved: Record<string, string>;
      keys: { key: string; hint: string }[];
      note: string;
    }>("/api/settings"),
  saveSettings: (body: {
    auditRetentionDays?: number;
    dbMaxMb?: number;
    prometheusUrl?: string;
    sqlRunnerJar?: string;
    sqlRunnerArgsStyle?: string;
  }) => request<{ ok: boolean }>("/api/settings", { method: "POST", body: JSON.stringify(body) }),
  resetSettings: () =>
    request<{ ok: boolean; note: string }>("/api/settings/reset", { method: "POST" }),
  credential: (cluster: string) =>
    request<{
      cluster: string;
      inCluster: boolean;
      apiServer: string | null;
      readonly: boolean;
      jobCount: number;
      note: string;
    }>(`/api/clusters/${cluster}/credential`),
};
// ---------------------------------------------------------------- 告警

export type AlertRule = {
  id: string;
  name: string;
  kind: string;
  threshold?: number;
  severity: string;
  enabled: boolean;
  createdAt: string;
};

export type AlertEvent = {
  id: string;
  ruleId: string;
  ruleName: string;
  kind: string;
  cluster: string;
  namespace: string;
  job: string;
  severity: string;
  message: string;
  state: "OPEN" | "ACK";
  ts: string;
};

export const alerts = {
  rules: () => request<{ items: AlertRule[]; kinds: string[] }>("/api/alerts/rules"),
  saveRule: (body: {
    id?: string;
    name: string;
    kind: string;
    threshold?: number;
    severity?: string;
    enabled?: boolean;
  }) =>
    request<{ ok: boolean; rule: AlertRule }>("/api/alerts/rules", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  removeRule: (id: string) =>
    request<{ ok: boolean }>(`/api/alerts/rules/${id}`, { method: "DELETE" }),
  events: (state?: string) =>
    request<{ items: AlertEvent[]; open: number }>(
      `/api/alerts/events${state ? `?state=${state}` : ""}`,
    ),
  ack: (id: string) => request<{ ok: boolean }>(`/api/alerts/events/${id}/ack`, { method: "POST" }),
};
export const observability = {
  applyPodMonitor: (yaml: string, cluster?: string) =>
    request<{ ok: boolean; cluster: string; namespace: string; name: string; hint: string }>(
      "/api/observability/podmonitor",
      { method: "POST", body: JSON.stringify({ yaml, cluster }) },
    ),
  patchLabels: (
    opts: { labels?: Record<string, string>; onlyNodeport?: boolean; cluster?: string } = {},
  ) =>
    request<{ ok: boolean; patched: number; failed: number; total: number }>(
      "/api/observability/patch-labels",
      { method: "POST", body: JSON.stringify(opts) },
    ),
};
export const clusterAdmin = {
  reconnect: (id: string) =>
    request<{ ok: boolean; cluster: string; jobCount: number }>(`/api/clusters/${id}/reconnect`, {
      method: "POST",
    }),
  setReadonly: (id: string, readonly: boolean) =>
    request<{ ok: boolean; cluster: string; readonly: boolean; persisted: boolean }>(
      `/api/clusters/${id}`,
      {
        method: "PATCH",
        body: JSON.stringify({ readonly }),
      },
    ),
  add: (body: {
    id: string;
    kubeconfigPath?: string;
    /** 直接粘贴 kubeconfig 内容（与 kubeconfigPath 二选一，优先这个） */
    kubeconfig?: string;
    readonly?: boolean;
  }) =>
    request<{ ok: boolean; cluster: string; jobCount: number; supportedKinds: string[] }>(
      "/api/clusters",
      { method: "POST", body: JSON.stringify(body) },
    ),
  /** 接入前测试连通性：不落库、不改集群 */
  test: (body: { kubeconfigPath?: string; kubeconfig?: string }) =>
    request<{
      ok: boolean;
      serverVersion: string;
      jobCount: number;
      supportedKinds: string[];
      latencyMs: number;
    }>("/api/clusters/test", { method: "POST", body: JSON.stringify(body) }),
};
export type TemplateItem = {
  id: string;
  name: string;
  kind: string;
  sourceJob?: string;
  payload: Record<string, unknown>;
  createdAt: string;
};

export const templates = {
  list: () => request<{ items: TemplateItem[] }>("/api/templates"),
  createFromJob: (name: string, job: { cluster: string; namespace: string; name: string }) =>
    request<{ ok: boolean; template: TemplateItem }>("/api/templates", {
      method: "POST",
      body: JSON.stringify({ name, fromJob: job }),
    }),
  /** 直接给 payload（手写 / 从 YAML 导入） */
  create: (name: string, payload: Record<string, unknown>) =>
    request<{ ok: boolean; template: TemplateItem }>("/api/templates", {
      method: "POST",
      body: JSON.stringify({ name, payload }),
    }),
  remove: (id: string) => request<{ ok: boolean }>(`/api/templates/${id}`, { method: "DELETE" }),
  update: (id: string, body: { name?: string; payload?: Record<string, unknown> }) =>
    request<{ ok: boolean; template: TemplateItem }>(`/api/templates/${id}`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
};
export const sqlApi = {
  execute: (body: {
    cluster?: string;
    namespace?: string;
    session: string;
    sql: string;
    maxRows?: number;
  }) =>
    request<{
      ok: boolean;
      gateway: string;
      columns: string[];
      rows: unknown[][];
      rowCount: number;
      truncated: boolean;
      tookMs: number;
    }>("/api/sql/execute", { method: "POST", body: JSON.stringify(body) }),
  gatewayInfo: (cluster: string, ns: string, session: string) =>
    request<{
      found: boolean;
      up?: boolean;
      url?: string;
      info?: Record<string, unknown>;
      error?: string;
    }>(`/api/sql/gateway/${cluster}/${ns}/${session}`),
};
// MCP：机器 Token 与一次性审批（docs/mcp-spec.md §9）
export const mcp = {
  tokens: () => request<{ items: unknown[] }>("/api/tokens"),
  createToken: (body: {
    name: string;
    scopes: string[];
    allowNamespaces: string[];
    allowJobs: string[];
    qps?: number;
    expiresInDays?: number;
  }) =>
    request<{ id: string; token: string; note: string }>("/api/tokens", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  revokeToken: (id: string) => request<{ ok: boolean }>(`/api/tokens/${id}`, { method: "DELETE" }),
  approvals: (status?: string) =>
    request<{ items: unknown[] }>(`/api/approvals${status ? `?status=${status}` : ""}`),
  decide: (id: string, decision: "approve" | "reject") =>
    request<{ ok: boolean; approvalToken?: string; note: string }>(`/api/approvals/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ decision }),
    }),
};
