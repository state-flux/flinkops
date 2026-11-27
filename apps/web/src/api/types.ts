// 后端 JSON 在前端的显式类型。
//
// 为什么单独一个文件：视图里曾有 39 处 `as any` 直接读 `?.cluster`、`?.items`，
// 后端字段改名时编译器完全不报警，只有用户点进页面才发现空白。
// 这里只声明**前端真正读取**的字段；分页 envelope 与错误体也放这里，避免各处重复写。

export type Paged<T> = {
  items: T[];
  total?: number;
  count?: number;
  limit?: number;
  offset?: number;
  asOf?: string;
  indexAgeSecs?: number;
};

/** SQL 工作台的作业（后端 `flinkops_sql_jobs`）。 */
export type SqlJob = {
  id: string;
  name: string;
  cluster: string;
  namespace: string;
  session?: string;
  sqlText?: string;
  version?: number;
  state?: string;
  jobId?: string | null;
  lastError?: string | null;
  createdAt?: string;
  updatedAt?: string;
};

export type SqlVersion = {
  id?: string;
  version: number;
  sqlText?: string;
  createdAt?: string;
};

/** 数据源（catalog）列表项。 */
export type CatalogItem = {
  id: string;
  name: string;
  kind: string;
  dsn?: string;
  tables?: number;
  syncedAt?: string | null;
};

/**
 * 环境基线：本质是"提交时的一坨默认值"，字段随产品演进增删。
 * 用索引签名而不是逐个列字段 —— 否则每加一个预设项都要改类型（本来就该自由扩展）。
 */
export type PresetItem = {
  id: string;
  name: string;
  builtin?: boolean;
  updatedAt?: string;
  /** 扁平 KV 的默认提交参数 */
  defaults?: Record<string, unknown>;
  [key: string]: unknown;
};

/** 校验结果条目（editor / submit 共用）。 */
export type Finding = {
  severity: "error" | "warn" | "info" | string;
  code: string;
  path?: string;
  message: string;
};

/** 快照 / checkpoint 列表项。 */
export type SnapshotItem = {
  id?: string;
  name?: string;
  kind?: string;
  cluster?: string;
  namespace?: string;
  jobName?: string;
  jobId?: string;
  state?: string;
  path?: string;
  timestamp?: string;
  sizeBytes?: number;
};

/** 作业 spec 视图（详情页编辑用）。 */
export type JobSpecView = {
  image?: string;
  parallelism?: number;
  taskManagerReplicas?: number;
  flinkVersion?: string;
  restartNonce?: number;
  upgradeMode?: string;
  [key: string]: unknown;
};

/** 实时指标 / 背压 / 火焰图这些"结构随 Flink 版本变"的响应。 */
export type LooseJson = Record<string, unknown>;

/** 统一取错误文案（未知错误也有兜底，不再靠 `(e: any).message`）。 */
export function errMsg(e: unknown, fallback = "操作失败"): string {
  if (typeof e === "string" && e.trim()) return e;
  if (e && typeof e === "object" && "message" in e) {
    const m = (e as { message?: unknown }).message;
    if (typeof m === "string" && m.trim()) return m;
  }
  return fallback;
}
