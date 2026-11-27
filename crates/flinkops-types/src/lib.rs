//! Shared domain types for FlinkOps.
//!
//! 设计要点：
//! - 所有 DTO 都是 `camelCase`，直接给前端用，避免在 API 层做字段名转换。
//! - CRD 读取走 `serde_json::Value`（动态客户端），所以这里提供一组
//!   "从任意 FlinkDeployment JSON 里提取字段" 的纯函数，可单测、不依赖 kube。
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod crd;

// ---------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("upstream {target}: {message}")]
    Upstream { target: String, message: String },
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn status_code(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::BadRequest(_) => 400,
            Error::Unauthorized => 401,
            Error::Forbidden(_) => 403,
            Error::Conflict(_) => 409,
            Error::Upstream { .. } => 502,
            Error::Internal(_) => 500,
        }
    }

    /// 面向用户的消息：不带 `bad request:` / `conflict:` 这类机器前缀
    /// （HTTP 层已经有 `code` 字段了，message 里再重复一遍只是噪音）。
    pub fn message(&self) -> String {
        match self {
            Error::NotFound(s) => s.clone(),
            Error::BadRequest(s) => s.clone(),
            Error::Unauthorized => "未登录或会话已失效，请重新登录".into(),
            Error::Forbidden(s) => s.clone(),
            Error::Conflict(s) => s.clone(),
            Error::Upstream { target, message } => format!("{target}: {message}"),
            Error::Internal(s) => s.clone(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::BadRequest(_) => "bad_request",
            Error::Unauthorized => "unauthorized",
            Error::Forbidden(_) => "forbidden",
            Error::Conflict(_) => "conflict",
            Error::Upstream { .. } => "upstream_error",
            Error::Internal(_) => "internal",
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("json: {e}"))
    }
}

// ---------------------------------------------------------------- cluster

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterRef {
    pub id: String,
    pub name: String,
    pub api_server: String,
    /// read-write | readonly
    #[serde(default)]
    pub mode: String,
}

// ---------------------------------------------------------------- job

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub last_transition_time: Option<String>,
}

/// 列表页用的一行。字段全部来自 CRD，不掺运行时数据。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    /// FlinkDeployment | FlinkSessionJob
    #[serde(default)]
    pub kind: String,
    /// RUNNING / FAILED / RECONCILING / SUSPENDED …
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub parallelism: Option<i64>,
    /// job 图实际并行度（取 REST vertices 的最大 parallelism）。
    ///
    /// `spec.job.parallelism` 只是入口算子的并行度：实测某作业 spec 写 1，
    /// job 图里是 24（6 TM × 4 slots）。排障时按 spec 判断会误以为资源浪费。
    #[serde(default)]
    pub parallelism_actual: Option<i64>,
    #[serde(default)]
    pub task_manager_replicas: Option<i64>,
    #[serde(default)]
    pub slots_per_tm: Option<i64>,
    #[serde(default)]
    pub restart_nonce: Option<i64>,
    #[serde(default)]
    pub upgrade_mode: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub flink_version: Option<String>,
    /// spec.jobManager.resource（列表里也要能核对，不必点进详情）
    #[serde(default)]
    pub job_manager_cpu: Option<String>,
    #[serde(default)]
    pub job_manager_memory: Option<String>,
    #[serde(default)]
    pub task_manager_cpu: Option<String>,
    #[serde(default)]
    pub task_manager_memory: Option<String>,
    /// ClusterIP | NodePort | LoadBalancer | None
    #[serde(default)]
    pub rest_exposed: Option<String>,
    #[serde(default)]
    pub job_manager_status: Option<String>,
    #[serde(default)]
    pub reconciliation_state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub generation: Option<i64>,
    #[serde(default)]
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// 老 operator 的关键信号（如 STABLE）；新版本可能为空
    #[serde(default)]
    pub lifecycle_state: Option<String>,
    /// jobStatus.startTime（毫秒 → RFC3339）
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl JobSummary {
    pub fn slots_total(&self) -> Option<i64> {
        match (self.task_manager_replicas, self.slots_per_tm) {
            (Some(r), Some(s)) => Some(r * s),
            _ => None,
        }
    }

    /// 槽位利用率（0.0–1.0+）。>1 说明并行度已经超过槽位总量。
    pub fn slot_pressure(&self) -> Option<f64> {
        let total = self.slots_total()?;
        let p = self.parallelism?;
        if total <= 0 {
            return None;
        }
        Some(p as f64 / total as f64)
    }

    /// Operator 认为"真的在跑"的唯一可信信号。
    pub fn running_condition(&self) -> Option<&Condition> {
        self.conditions
            .iter()
            .find(|c| c.kind == "Running" || c.kind == "JobRunning")
    }

    /// 是否"真的在跑"。
    ///
    /// 新 operator 写 `status.conditions[Running=True]`；老版本（如 1.15 那批）
    /// **完全不写 conditions**，只能用 `lifecycleState=STABLE` + `jobManagerReady` 兜底。
    /// 这是真实集群里踩出来的差异，不要改回单一判据。
    /// 列表/详情共用的健康度：green / yellow / red。
    /// 放在 types 里，保证 API、CLI、前端看到的是同一个判断。
    pub fn health_level(&self) -> &'static str {
        // 过渡态：升级/回滚/调谐中，谁也不是"健康"也不是"失败"
        let lifecycle = self
            .lifecycle_state
            .as_deref()
            .unwrap_or("")
            .to_ascii_uppercase();
        if matches!(lifecycle.as_str(), "UPGRADING" | "ROLLING_BACK") {
            return "yellow";
        }
        // 终态成功：批作业跑完是正常的
        if matches!(
            self.state.to_ascii_uppercase().as_str(),
            "FINISHED" | "SUCCEEDED"
        ) {
            return "green";
        }
        // 被取消不是"健康"：作业确实停了，运维需要看见
        if matches!(
            self.state.to_ascii_uppercase().as_str(),
            "CANCELED" | "CANCELLED"
        ) {
            return "yellow";
        }
        if self.state.eq_ignore_ascii_case("FAILED") {
            return "red";
        }
        // status.error 是"最后一次 reconcile 的错误"，operator 不一定及时清空：
        // 只有在确实处于失败/回滚态时才据此判红，否则会出现"运行中的作业一片红"。
        if self.error.is_some() {
            let lifecycle = self
                .lifecycle_state
                .as_deref()
                .unwrap_or("")
                .to_ascii_uppercase();
            if matches!(lifecycle.as_str(), "FAILED" | "ROLLING_BACK") {
                return "red";
            }
        }
        if self.is_healthy() {
            "green"
        } else {
            "yellow"
        }
    }

    pub fn is_healthy(&self) -> bool {
        if self.state.eq_ignore_ascii_case("FAILED") {
            return false;
        }
        if let Some(c) = self.running_condition() {
            return c.status.eq_ignore_ascii_case("true");
        }
        let jm_ready = self
            .job_manager_status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("READY"))
            .unwrap_or(false);
        // 老 operator 根本不写 lifecycleState：不能因为缺字段就判黄
        let stable = self
            .lifecycle_state
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("STABLE"))
            .unwrap_or(true);
        jm_ready && stable && self.state.eq_ignore_ascii_case("RUNNING")
    }
}

/// 详情页的 spec 视图（只挑前端要展示/编辑的字段）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobSpecView {
    pub image: Option<String>,
    pub flink_version: Option<String>,
    pub service_account: Option<String>,
    pub restart_nonce: Option<i64>,
    pub parallelism: Option<i64>,
    pub upgrade_mode: Option<String>,
    pub job_state: Option<String>,
    pub jar_uri: Option<String>,
    pub entry_class: Option<String>,
    pub args: Vec<String>,
    pub task_manager_replicas: Option<i64>,
    pub job_manager_cpu: Option<String>,
    pub job_manager_memory: Option<String>,
    pub task_manager_cpu: Option<String>,
    pub task_manager_memory: Option<String>,
    #[serde(default)]
    pub flink_configuration: BTreeMap<String, String>,
    /// ClusterIP | NodePort | LoadBalancer（来自 flinkConfiguration）
    pub rest_exposed: Option<String>,
    pub ingress_template: Option<String>,
    /// 原样返回，供 CRD 编辑器使用
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotView {
    pub id: Option<i64>,
    pub path: Option<String>,
    pub size: Option<i64>,
    pub duration_ms: Option<i64>,
    pub timestamp: Option<i64>,
    pub trigger_type: Option<String>,
    pub format_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointView {
    pub last: Option<SnapshotView>,
    pub completed: i64,
    pub failed: i64,
    pub restored: i64,
    pub in_progress: i64,
    pub last_periodic_ts: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SavepointView {
    pub last: Option<SnapshotView>,
    pub history: Vec<SnapshotView>,
    pub trigger_id: Option<String>,
    pub last_periodic_ts: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskManagerView {
    pub replicas: i64,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobDetail {
    pub summary: JobSummary,
    /// green / yellow / red（与列表一致）
    #[serde(default)]
    pub health: String,
    pub spec: JobSpecView,
    pub checkpoint: CheckpointView,
    pub savepoint: SavepointView,
    pub task_manager: TaskManagerView,
    pub cluster_info: BTreeMap<String, String>,
}

// ---------------------------------------------------------------- metrics

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LiveMetrics {
    pub job_id: String,
    pub values: BTreeMap<String, f64>,
    /// Flink REST（毫秒级）| prometheus（15–60s）
    pub source: String,
    pub sampled_at: String,
    /// 请求失败时的降级原因（前端据此显示"实时源不可用"）
    #[serde(default)]
    pub degraded: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Backpressure {
    pub level: String,
    pub ratio: f64,
    pub subtasks: Vec<SubtaskBackpressure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SubtaskBackpressure {
    pub subtask: i64,
    pub level: String,
    pub ratio: f64,
}

// ---------------------------------------------------------------- findings

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
    Info,
}

/// 校验/巡检的结构化结果，前端直接渲染成"校验"列表。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    /// JSON 路径，如 spec.job.parallelism
    pub path: String,
    pub message: String,
}

impl Finding {
    pub fn error(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
    pub fn warn(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
    pub fn info(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpecDiff {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

// ---------------------------------------------------------------- profile

/// 环境基线：一切"换团队就会变"的东西沉在这里。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub flink_version: String,
    pub image: String,
    pub namespace: String,
    pub checkpoint_prefix: String,
    pub savepoint_prefix: String,
    pub state_backend: String,
    /// ClusterIP | Ingress | NodePort
    pub rest_exposed: String,
    pub metrics_port: u16,
    pub service_account: String,
    #[serde(default)]
    pub defaults: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub builtin: bool,
}

impl Default for Profile {
    fn default() -> Self {
        let mut defaults = BTreeMap::new();
        defaults.insert("execution.checkpointing.interval".into(), "60s".into());
        defaults.insert("execution.checkpointing.mode".into(), "EXACTLY_ONCE".into());
        defaults.insert("execution.checkpointing.timeout".into(), "10min".into());
        defaults.insert("state.backend.incremental".into(), "true".into());
        defaults.insert("restart-strategy.type".into(), "exponential-delay".into());
        defaults.insert(
            "kubernetes.operator.savepoint.history.max.count".into(),
            "10".into(),
        );
        defaults.insert(
            "kubernetes.operator.savepoint.history.max.age".into(),
            "168 h".into(),
        );
        Self {
            id: "general-1.19".into(),
            name: "general-1.19".into(),
            flink_version: "v1_19".into(),
            image: "ghcr.io/flink-operator/flink:1.19.1-java17".into(),
            namespace: "flink-jobs".into(),
            checkpoint_prefix: "s3://flink-state/checkpoints/{name}".into(),
            savepoint_prefix: "s3://flink-state/savepoints/{name}".into(),
            state_backend: "rocksdb".into(),
            rest_exposed: "ClusterIP".into(),
            metrics_port: 9250,
            service_account: "flink-sa".into(),
            defaults,
            labels: BTreeMap::from([
                ("flinkops.io/scrape".to_string(), "true".to_string()),
                (
                    "app.kubernetes.io/managed-by".to_string(),
                    "flinkops".to_string(),
                ),
            ]),
            builtin: true,
        }
    }
}

// ---------------------------------------------------------------- audit

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: String,
    pub ts: String,
    pub actor: String,
    /// job.start / job.stop / savepoint.trigger / ui.open / proxy.blocked …
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub cluster: String,
    #[serde(default)]
    pub namespace: String,
    /// ok | denied | error
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
}

// ---------------------------------------------------------------- session

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionCluster {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub state: String,
    pub flink_version: Option<String>,
    pub image: Option<String>,
    pub job_manager_cpu: Option<String>,
    pub job_manager_memory: Option<String>,
    pub task_manager_replicas: i64,
    pub slots_per_tm: i64,
    pub used_slots: Option<i64>,
    /// 空闲回收 TTL（来自 flinkConfiguration 或平台注解）
    pub idle_ttl: Option<String>,
    pub sql_gateway: Option<String>,
}

// ---------------------------------------------------------------- events

/// K8s Event（只保留排障需要的字段）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    /// Normal | Warning
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub first_timestamp: Option<String>,
    #[serde(default)]
    pub last_timestamp: Option<String>,
    /// 事件来源（kubelet / flink-operator …）
    #[serde(default)]
    pub source: Option<String>,
}

// ---------------------------------------------------------------- snapshots

/// FlinkStateSnapshot 的列表行。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotListItem {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    #[serde(default)]
    pub job_name: Option<String>,
    /// COMPLETED | IN_PROGRESS | FAILED | UNKNOWN
    pub state: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------- helpers

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `v1_19` / `v1_9` → `1.19` / `1.9`
pub fn normalize_flink_version(raw: &str) -> String {
    raw.trim_start_matches('v').replace('_', ".")
}
