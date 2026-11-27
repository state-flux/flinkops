//! 从 FlinkDeployment / FlinkSessionJob 的 JSON 中提取领域字段。
//!
//! 为什么用 `Value` 而不是 derive 的强类型：平台要能对接任何版本的
//! flink-kubernetes-operator（v1beta1 至今有 20+ 字段演进），动态提取
//! 比跟版本绑定的类型更耐用；强类型只在本平台的 API 契约里用。
use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    normalize_flink_version, CheckpointView, Condition, JobDetail, JobSpecView, JobSummary,
    SavepointView, SnapshotView, TaskManagerView,
};

pub const GROUP: &str = "flink.apache.org";
pub const VERSION: &str = "v1beta1";
pub const KIND_DEPLOYMENT: &str = "FlinkDeployment";
pub const KIND_SESSION_JOB: &str = "FlinkSessionJob";
pub const KIND_SNAPSHOT: &str = "FlinkStateSnapshot";
pub const KIND_BLUE_GREEN: &str = "FlinkBlueGreenDeployment";

/// 按路径取 JSON 节点。
pub fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

/// 取字符串；数字/布尔也会被转成字符串（CRD 里很多数值是 string）。
pub fn s(v: &Value, path: &[&str]) -> Option<String> {
    match at(v, path)? {
        Value::String(x) => {
            if x.is_empty() {
                None
            } else {
                Some(x.clone())
            }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn i(v: &Value, path: &[&str]) -> Option<i64> {
    match at(v, path)? {
        Value::Number(n) => n.as_i64(),
        Value::String(x) => x.trim().parse::<i64>().ok(),
        _ => None,
    }
}

pub fn f(v: &Value, path: &[&str]) -> Option<f64> {
    match at(v, path)? {
        Value::Number(n) => n.as_f64(),
        Value::String(x) => x.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub fn str_map(v: &Value, path: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(map)) = at(v, path) {
        for (k, val) in map {
            let text = match val {
                Value::String(x) => x.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => continue,
                other => other.to_string(),
            };
            out.insert(k.clone(), text);
        }
    }
    out
}

/// operator 把 startTime/updateTime 写成毫秒时间戳字符串（如 "1785146781543"）。
/// 直接透传会让前端算出 5 万年后，这里统一转 RFC3339。
pub fn epoch_millis_to_rfc3339(raw: Option<String>) -> Option<String> {
    let ms: i64 = raw?.trim().parse().ok()?;
    if ms <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

pub fn str_vec(v: &Value, path: &[&str]) -> Vec<String> {
    match at(v, path) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn conditions(v: &Value) -> Vec<Condition> {
    match at(v, &["status", "conditions"]) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|c| Condition {
                kind: s(c, &["type"]).unwrap_or_default(),
                status: s(c, &["status"]).unwrap_or_default(),
                reason: s(c, &["reason"]).unwrap_or_default(),
                message: s(c, &["message"]).unwrap_or_default(),
                last_transition_time: s(c, &["lastTransitionTime"]),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// 规范化作业状态。
///
/// 优先级（踩过的坑）：
/// 1. `spec.job.state=suspended` 是**用户期望态**，必须最先返回 —— 否则 status 里
///    残留的 `RUNNING`/`FINISHED` 会让"已挂起"显示成"运行中/已结束"；
/// 2. `jobManagerDeploymentStatus=ERROR` 有否决权（JM 挂了但 jobStatus 还写着 RUNNING）；
/// 3. 其余按 `jobStatus.state` → JM 状态 → spec 兜底。
pub fn derive_state(v: &Value) -> String {
    let spec_state = s(v, &["spec", "job", "state"]).unwrap_or_default();
    let suspended = spec_state.eq_ignore_ascii_case("suspended");
    let jm = s(v, &["status", "jobManagerDeploymentStatus"]);

    // session cluster（没有 spec.job）：operator 会把 jobStatus.state 标成 FINISHED，
    // 那是"没有作业可跑"的意思，不代表集群挂了 → 改看 JM 状态。
    let is_session = v.get("spec").and_then(|s| s.get("job")).is_none();

    if jm.as_deref() == Some("ERROR") {
        return "FAILED".to_string();
    }
    if suspended {
        return "SUSPENDED".to_string();
    }
    if jm.as_deref() == Some("MISSING") {
        // JM 被删且用户没要求挂起 → 异常缺失，不是"调谐中"
        return "FAILED".to_string();
    }
    // 过渡态优先：升级/回滚中 `jobStatus.state` 往往先变成 FINISHED/CANCELED，
    // 直接用它会让列表显示"FINISHED + 绿点"，运维以为作业被取消了。
    let lifecycle = s(v, &["status", "lifecycleState"])
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(lifecycle.as_str(), "UPGRADING" | "ROLLING_BACK") {
        return lifecycle;
    }
    if !is_session {
        if let Some(state) = s(v, &["status", "jobStatus", "state"]) {
            return state;
        }
    }
    match jm.as_deref() {
        Some("READY") => "RUNNING".to_string(),
        Some("DEPLOYING") | Some("DEPLOYED_NOT_READY") => "RECONCILING".to_string(),
        Some(other) => other.to_string(),
        None => match spec_state.as_str() {
            "running" => "RECONCILING".to_string(),
            _ => "UNKNOWN".to_string(),
        },
    }
}

/// 列表页一行。
pub fn summary(cluster: &str, kind: &str, obj: &Value) -> JobSummary {
    let cfg = str_map(obj, &["spec", "flinkConfiguration"]);
    let slots = cfg
        .get("taskmanager.numberOfTaskSlots")
        .and_then(|x| x.trim().parse::<i64>().ok());
    let exposed = cfg
        .get("kubernetes.rest-service.exposed.type")
        .cloned()
        .unwrap_or_else(|| "ClusterIP".to_string());

    JobSummary {
        cluster: cluster.to_string(),
        namespace: s(obj, &["metadata", "namespace"]).unwrap_or_else(|| "default".into()),
        name: s(obj, &["metadata", "name"]).unwrap_or_default(),
        kind: kind.to_string(),
        state: derive_state(obj),
        job_id: s(obj, &["status", "jobStatus", "jobId"]),
        parallelism: i(obj, &["spec", "job", "parallelism"])
            .or_else(|| i(obj, &["spec", "parallelism"])),
        // 由 REST 侧在 detail 里补；CRD 本身没有这个信息
        parallelism_actual: None,
        task_manager_replicas: i(obj, &["spec", "taskManager", "replicas"]),
        slots_per_tm: slots,
        restart_nonce: i(obj, &["spec", "restartNonce"]),
        upgrade_mode: s(obj, &["spec", "job", "upgradeMode"]),
        image: s(obj, &["spec", "image"]),
        flink_version: s(obj, &["spec", "flinkVersion"]).map(|v| normalize_flink_version(&v)),
        job_manager_cpu: qty(obj, &["spec", "jobManager", "resource", "cpu"]),
        job_manager_memory: qty(obj, &["spec", "jobManager", "resource", "memory"]),
        task_manager_cpu: qty(obj, &["spec", "taskManager", "resource", "cpu"]),
        task_manager_memory: qty(obj, &["spec", "taskManager", "resource", "memory"]),
        rest_exposed: Some(exposed),
        job_manager_status: s(obj, &["status", "jobManagerDeploymentStatus"]),
        reconciliation_state: s(obj, &["status", "reconciliationStatus", "state"]),
        error: s(obj, &["status", "error"]),
        generation: i(obj, &["metadata", "generation"]),
        observed_generation: i(obj, &["status", "observedGeneration"]),
        conditions: conditions(obj),
        lifecycle_state: s(obj, &["status", "lifecycleState"]),
        started_at: epoch_millis_to_rfc3339(s(obj, &["status", "jobStatus", "startTime"])),
        updated_at: epoch_millis_to_rfc3339(s(obj, &["status", "jobStatus", "updateTime"]))
            .or_else(|| s(obj, &["status", "jobStatus", "updateTime"])),
    }
}

/// 递归打码：字段名/环境变量名像凭据的值一律打码，operator 存进 status 的
/// 整段 spec 字符串直接省略（里面带明文 AK/SK）。
///
/// 放在 types 里，保证 `/api/jobs/{..}`、`/yaml`、MCP 工具输出用的是同一套规则。
pub fn redact_sensitive(v: &mut serde_json::Value) {
    const NEEDLES: [&str; 6] = [
        "ACCESS_KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "PRIVATE_KEY",
    ];

    fn looks_sensitive(name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        NEEDLES.iter().any(|n| upper.contains(n))
    }

    match v {
        serde_json::Value::Object(map) => {
            let key_name = map
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let is_env_entry = looks_sensitive(&key_name);
            for (k, val) in map.iter_mut() {
                if k == "lastReconciledSpec" || k == "lastStableSpec" {
                    *val = serde_json::Value::String("<omitted: 含完整 spec 与凭据>".into());
                    continue;
                }
                if is_env_entry && k == "value" && val.is_string() {
                    *val = serde_json::Value::String("****".into());
                    continue;
                }
                if looks_sensitive(k) && val.is_string() {
                    *val = serde_json::Value::String("****".into());
                    continue;
                }
                if let serde_json::Value::String(sv) = val {
                    // 只处理"整份对象被打包成字符串"的情况（kubectl 的
                    // last-applied-configuration、operator 的 lastStableSpec）。
                    // 不能对任意字符串做子串匹配：env 的 name 字段本身就是
                    // AWS_SECRET_ACCESS_KEY，会被误吞成占位符，写回时直接毁配置。
                    let is_blob =
                        sv.trim_start().starts_with('{') || sv.trim_start().starts_with('[');
                    if k.contains("last-applied-configuration")
                        || (is_blob
                            && (sv.contains("ACCESS_KEY")
                                || sv.contains("SECRET_ACCESS_KEY")
                                || sv.contains("PRIVATE KEY")))
                    {
                        *val = serde_json::Value::String("<omitted: 含完整对象与凭据>".into());
                        continue;
                    }
                }
                redact_sensitive(val);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_sensitive),
        _ => {}
    }
}

/// 详情页的 spec 视图。
///
/// `raw` 会做**敏感值打码**：CR 里常见 `initContainer.env` 的 AK/SK，
/// 详情接口 / MCP 工具输出 / 截图都会把它带出去。YAML 接口与详情接口
/// 必须共用同一套打码逻辑，否则"一处脱敏、另一处明文"。
pub fn spec_view(obj: &Value) -> JobSpecView {
    let mut spec = at(obj, &["spec"]).cloned().unwrap_or(Value::Null);
    redact_sensitive(&mut spec);
    JobSpecView {
        image: s(obj, &["spec", "image"]),
        flink_version: s(obj, &["spec", "flinkVersion"]).map(|v| normalize_flink_version(&v)),
        job_manager_cpu: qty(obj, &["spec", "jobManager", "resource", "cpu"]),
        job_manager_memory: qty(obj, &["spec", "jobManager", "resource", "memory"]),
        task_manager_cpu: qty(obj, &["spec", "taskManager", "resource", "cpu"]),
        task_manager_memory: qty(obj, &["spec", "taskManager", "resource", "memory"]),
        service_account: s(obj, &["spec", "serviceAccount"]),
        restart_nonce: i(obj, &["spec", "restartNonce"]),
        parallelism: i(obj, &["spec", "job", "parallelism"]),
        upgrade_mode: s(obj, &["spec", "job", "upgradeMode"]),
        job_state: s(obj, &["spec", "job", "state"]),
        jar_uri: s(obj, &["spec", "job", "jarURI"]),
        entry_class: s(obj, &["spec", "job", "entryClass"]),
        args: str_vec(obj, &["spec", "job", "args"]),
        task_manager_replicas: i(obj, &["spec", "taskManager", "replicas"]),
        flink_configuration: str_map(obj, &["spec", "flinkConfiguration"]),
        rest_exposed: str_map(obj, &["spec", "flinkConfiguration"])
            .get("kubernetes.rest-service.exposed.type")
            .cloned()
            .or_else(|| Some("ClusterIP".to_string())),
        ingress_template: s(obj, &["spec", "ingress", "template"]),
        raw: spec,
    }
}

fn snapshot_view(v: Option<&Value>) -> Option<SnapshotView> {
    let v = v?;
    Some(SnapshotView {
        id: i(v, &["id"]).or_else(|| i(v, &["checkpointId"])),
        path: s(v, &["location"])
            .or_else(|| s(v, &["externalPath"]))
            .or_else(|| s(v, &["path"])),
        size: i(v, &["size"]),
        duration_ms: i(v, &["duration"]),
        // operator 的 lastSavepoint 用的是 timeStamp（大写 S）
        timestamp: i(v, &["timestamp"]).or_else(|| i(v, &["timeStamp"])),
        trigger_type: s(v, &["triggerType"]).or_else(|| s(v, &["trigger"])),
        format_type: s(v, &["formatType"]),
    })
}

/// 注意：`completed/failed/restored` 在 CRD 里**没有**，只能来自 Flink REST
/// 的 `/jobs/:id/checkpoints`。这里先给 0，由 API 层合并实时数据。
pub fn checkpoint_view(obj: &Value) -> CheckpointView {
    let ci = at(obj, &["status", "jobStatus", "checkpointInfo"]);
    CheckpointView {
        last: ci.and_then(|c| snapshot_view(c.get("lastCheckpoint"))),
        completed: 0,
        failed: 0,
        restored: 0,
        in_progress: 0,
        last_periodic_ts: ci.and_then(|c| i(c, &["lastPeriodicCheckpointTimestamp"])),
    }
}

pub fn savepoint_view(obj: &Value) -> SavepointView {
    let si = at(obj, &["status", "jobStatus", "savepointInfo"]);
    let history = match si.and_then(|s| s.get("savepointHistory")) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|x| snapshot_view(Some(x)))
            .collect(),
        _ => Vec::new(),
    };
    SavepointView {
        last: si.and_then(|s| snapshot_view(s.get("lastSavepoint"))),
        history,
        trigger_id: si.and_then(|s| s2(s, "triggerId")),
        last_periodic_ts: si.and_then(|s| i(s, &["lastPeriodicSavepointTimestamp"])),
    }
}

/// 资源量字段可能是数字（0.5）也可能是字符串（"0.5" / "1024m"），统一转字符串。
fn qty(obj: &Value, path: &[&str]) -> Option<String> {
    at(obj, path).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn s2(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}

pub fn task_manager_view(obj: &Value) -> TaskManagerView {
    TaskManagerView {
        replicas: i(obj, &["status", "taskManager", "replicas"]).unwrap_or(0),
        labels: str_map(obj, &["status", "taskManager", "labels"]),
    }
}

/// 组装详情页需要的全部内容。
pub fn detail(cluster: &str, obj: &Value) -> JobDetail {
    let kind = s(obj, &["kind"]).unwrap_or_else(|| KIND_DEPLOYMENT.to_string());
    let summary = summary(cluster, &kind, obj);
    let health = summary.health_level().to_string();
    JobDetail {
        health,
        summary,
        spec: spec_view(obj),
        checkpoint: checkpoint_view(obj),
        savepoint: savepoint_view(obj),
        task_manager: task_manager_view(obj),
        cluster_info: str_map(obj, &["status", "clusterInfo"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deployment() -> Value {
        json!({
            "apiVersion": "flink.apache.org/v1beta1",
            "kind": "FlinkDeployment",
            "metadata": { "name": "orders", "namespace": "flink-jobs", "generation": 7 },
            "spec": {
                "image": "ghcr.io/x/flink:1.19.1",
                "flinkVersion": "v1_19",
                "restartNonce": 3,
                "serviceAccount": "flink-sa",
                "flinkConfiguration": {
                    "taskmanager.numberOfTaskSlots": "2",
                    "kubernetes.rest-service.exposed.type": "NodePort",
                    "execution.checkpointing.interval": "60s"
                },
                "jobManager": { "resource": { "cpu": 1, "memory": "2Gi" } },
                "taskManager": { "replicas": 4, "resource": { "cpu": 2, "memory": "4Gi" } },
                "job": {
                    "jarURI": "s3://a/b.jar",
                    "entryClass": "com.example.Job",
                    "args": ["--env", "prod"],
                    "parallelism": 8,
                    "upgradeMode": "last-state",
                    "state": "running"
                }
            },
            "status": {
                "jobManagerDeploymentStatus": "READY",
                "reconciliationStatus": { "state": "DEPLOYED" },
                "observedGeneration": 7,
                "conditions": [
                    { "type": "Running", "status": "True", "reason": "JobRunning" },
                    { "type": "JobManagerReady", "status": "True" }
                ],
                // operator 真实写入位置：status.jobStatus.{checkpointInfo,savepointInfo}
                // （旧单测把这两段放在 status 下，正好掩盖了"详情页恒空"的 bug）
                "jobStatus": {
                    "state": "RUNNING", "jobId": "abc123", "updateTime": "2026-11-25T01:00:00Z",
                    "checkpointInfo": { "counts": { "completed": 1284, "failed": 3 }, "lastCheckpoint": { "id": 1284, "size": 13000000, "duration": 812 } },
                    "savepointInfo": { "lastSavepoint": { "id": 41, "location": "s3://sp/41" }, "savepointHistory": [ { "id": 41 } ] }
                },
                "taskManager": { "replicas": 4, "labels": { "app": "orders" } }
            }
        })
    }

    #[test]
    fn extracts_core_fields() {
        let s = summary("prod", KIND_DEPLOYMENT, &deployment());
        assert_eq!(s.name, "orders");
        assert_eq!(s.namespace, "flink-jobs");
        assert_eq!(s.state, "RUNNING");
        assert_eq!(s.job_id.as_deref(), Some("abc123"));
        assert_eq!(s.parallelism, Some(8));
        assert_eq!(s.task_manager_replicas, Some(4));
        assert_eq!(s.slots_per_tm, Some(2));
        assert_eq!(s.slots_total(), Some(8));
        assert_eq!(s.slot_pressure(), Some(1.0));
        assert_eq!(s.rest_exposed.as_deref(), Some("NodePort"));
        assert_eq!(s.flink_version.as_deref(), Some("1.19"));
        assert!(s.is_healthy());
        assert_eq!(s.generation, Some(7));
        assert_eq!(s.observed_generation, Some(7));
    }

    #[test]
    fn falls_back_to_spec_state_when_status_empty() {
        let mut v = deployment();
        v["status"] = json!({});
        let s = summary("prod", KIND_DEPLOYMENT, &v);
        assert_eq!(s.state, "RECONCILING");
        assert!(!s.is_healthy());
    }

    /// 真实集群（Flink 1.15.4 + 老 operator）的形态：没有 conditions、没有
    /// observedGeneration，只有 lifecycleState 与毫秒时间戳。
    #[test]
    fn legacy_operator_without_conditions_is_healthy() {
        let v = serde_json::json!({
            "metadata": { "name": "orders", "namespace": "flink", "generation": 11 },
            "spec": {
                "image": "harbor/flink:1.5", "flinkVersion": "v1_15",
                "flinkConfiguration": {
                    "taskmanager.numberOfTaskSlots": "1",
                    "kubernetes.rest-service.exposed.type": "NodePort"
                },
                "taskManager": { "replicas": 1 },
                "job": { "jarURI": "local:///a.jar", "parallelism": 1, "state": "running", "upgradeMode": "stateless" }
            },
            "status": {
                "clusterInfo": { "flink-version": "1.15.4" },
                "jobManagerDeploymentStatus": "READY",
                "lifecycleState": "STABLE",
                "jobStatus": {
                    "state": "RUNNING",
                    "jobId": "6ec3f5b6eeb3c84360e988e498c1caad",
                    "startTime": "1785146781543",
                    "updateTime": "1785146942923"
                }
            }
        });
        let s = summary("real", KIND_DEPLOYMENT, &v);
        assert_eq!(s.state, "RUNNING");
        assert_eq!(s.lifecycle_state.as_deref(), Some("STABLE"));
        assert!(s.is_healthy(), "无 conditions 时应靠 lifecycleState 兜底");
        assert_eq!(s.started_at.as_deref(), Some("2026-07-27T10:06:21Z"));
        assert_eq!(s.slots_per_tm, Some(1));
        assert_eq!(s.rest_exposed.as_deref(), Some("NodePort"));
    }

    #[test]
    fn detail_exposes_spec_and_snapshots() {
        let d = detail("prod", &deployment());
        assert_eq!(d.spec.jar_uri.as_deref(), Some("s3://a/b.jar"));
        assert_eq!(d.spec.args, vec!["--env", "prod"]);
        assert_eq!(d.spec.job_manager_memory.as_deref(), Some("2Gi"));
        // counts 来自 REST，CRD 里为 0
        assert_eq!(d.checkpoint.completed, 0);
        assert_eq!(d.checkpoint.last.as_ref().unwrap().id, Some(1284));
        assert_eq!(d.checkpoint.last.as_ref().unwrap().duration_ms, Some(812));
        assert_eq!(d.savepoint.history.len(), 1);
        assert_eq!(d.task_manager.replicas, 4);
        assert_eq!(d.cluster_info.len(), 0);
    }
}
