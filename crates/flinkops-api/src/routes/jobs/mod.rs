//! 作业相关：列表 / 详情 / 实时指标 / 日志 / 启停 / 重启 / savepoint。
//!
//! 数据源策略（见 docs/data-sources.md）：
//! - 列表与详情：CRD（K8s）为主，详情额外用 Flink REST 补 checkpoint 计数；
//! - 实时指标：Flink REST（毫秒级），失败时返回 `degraded` 而不是静默 0；
//! - 日志：Flink REST 的文件日志；K8s 默认只有 stdout，失败时提示改用 Pod logs。

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, JobTarget, RequireWrite};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use flinkops_core as core;
use flinkops_flink_rest::{LogTarget, LIVE_METRICS};
use flinkops_types::{crd, now_rfc3339, Error, JobDetail, LiveMetrics, Severity};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

mod lifecycle;
mod list;
mod ops;
mod submit;

// 路由表仍按 `jobs::xxx` 引用，拆分对外不可见
pub use lifecycle::*;
pub use list::*;
pub use ops::*;
pub use submit::*;

/// MCP 写操作闸门：浏览器会话直接放行；Bearer token 走 scope + 资源范围 + 一次性审批
/// （见 docs/mcp-spec.md §2.4 / §4）。
async fn gate(
    st: &Arc<AppState>,
    headers: &HeaderMap,
    tool: &str,
    ns: &str,
    name: &str,
    args: Value,
    need_approval: bool,
) -> ApiResult<()> {
    crate::mcp_guard::gate(st, headers, tool, ns, name, args, need_approval)
        .await
        .map_err(ApiError)
}

// ---------------------------------------------------------------- 列表

/// 给一个集群挂上 watch：作业/会话作业变化即刷新索引行（删除则删索引）。
///
/// 周期同步仍保留（5 分钟一次），作为 watch 断流期间的兜底对账。
pub fn spawn_cluster_watcher(
    st: Arc<AppState>,
    id: String,
    c: Arc<flinkops_k8s::Cluster>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    for kind in [crd::KIND_DEPLOYMENT, crd::KIND_SESSION_JOB] {
        let st = st.clone();
        let id = id.clone();
        handles.push(c.spawn_watcher(kind, move |op, obj| {
            let st = st.clone();
            let id = id.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                match op {
                    flinkops_k8s::WatchOp::Upsert => {
                        let s = crd::summary(&id, &kind, &obj);
                        if let Err(e) = st.store.upsert_job(&s).await {
                            warn!(cluster = %id, job = %s.name, error = %e, "watch upsert failed");
                        }
                        st.mark_index_fresh();
                    }
                    flinkops_k8s::WatchOp::Delete => {
                        let ns = crd::s(&obj, &["metadata", "namespace"]).unwrap_or_default();
                        let name = crd::s(&obj, &["metadata", "name"]).unwrap_or_default();
                        if !name.is_empty() {
                            let _ = st.store.delete_job(&id, &ns, &name).await;
                        }
                    }
                }
            });
        }));
    }
    handles
}

/// 归一化 savepoint 路径：Flink/operator 返回的目录都不带尾斜杠，
/// 平台只要求"绝对 URI"，尾斜杠由这里补，避免"同一路径两种写法"互相打架。
fn normalize_savepoint_path(path: &str) -> ApiResult<String> {
    let p = path.trim();
    if p.is_empty() {
        return Err(ApiError(Error::BadRequest("savepoint 路径不能为空".into())));
    }
    if !p.contains("://") {
        return Err(ApiError(Error::BadRequest(format!(
            "savepoint 路径需要是绝对 URI（如 hdfs://ns/path/ 或 s3://bucket/path/），收到 {p}"
        ))));
    }
    Ok(if p.ends_with('/') {
        p.to_string()
    } else {
        format!("{p}/")
    })
}

/// 等某个 lifecycleState（如 SUSPENDED）出现，最多等 `budget_s` 秒。
async fn wait_lifecycle(
    c: &flinkops_k8s::Cluster,
    ns: &str,
    name: &str,
    want: &str,
    budget_s: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(budget_s);
    loop {
        if let Ok(obj) = c.get_raw(crd::KIND_DEPLOYMENT, ns, name).await {
            let lc = crd::s(&obj, &["status", "lifecycleState"]).unwrap_or_default();
            if lc.eq_ignore_ascii_case(want) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// 等作业"换了一个 jobId 且已经 RUNNING"——这才是恢复真的发生了。
async fn wait_job_restarted(
    c: &flinkops_k8s::Cluster,
    ns: &str,
    name: &str,
    before: Option<&str>,
    budget_s: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(budget_s);
    loop {
        if let Ok(obj) = c.get_raw(crd::KIND_DEPLOYMENT, ns, name).await {
            let job = crd::s(&obj, &["status", "jobStatus", "jobId"]);
            let state = crd::s(&obj, &["status", "jobStatus", "state"]).unwrap_or_default();
            if state.eq_ignore_ascii_case("RUNNING") {
                if let Some(now) = job.as_deref() {
                    if before != Some(now) {
                        return true;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// 轮询 FlinkStateSnapshot 的 `status.state`，最多等 `budget_ms`。
/// 返回 (state, error)；一直没结论时返回 `IN_PROGRESS`。
async fn wait_snapshot_state(
    c: &flinkops_k8s::Cluster,
    ns: &str,
    name: &str,
    budget_ms: u64,
) -> (String, Option<String>) {
    let deadline = Instant::now() + std::time::Duration::from_millis(budget_ms);
    loop {
        if let Ok(obj) = c.get_raw(crd::KIND_SNAPSHOT, ns, name).await {
            let state = crd::s(&obj, &["status", "state"]).unwrap_or_else(|| "PENDING".into());
            if state != "PENDING" && state != "IN_PROGRESS" {
                return (state, crd::s(&obj, &["status", "error"]));
            }
        }
        if Instant::now() >= deadline {
            return ("IN_PROGRESS".into(), None);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// 写操作后立刻刷新该作业的索引行。
///
/// 列表读的是本地索引；不刷新的话，刚提交/刚扩容的作业要等到下一次全量
/// 同步才出现（前端 30s 轮询看起来"没反应"）。
async fn refresh_index(st: &Arc<AppState>, cluster: &str, ns: &str, name: &str) {
    let Ok(c) = st.cluster(cluster).await else {
        return;
    };
    match c.get_any(ns, name).await {
        Ok((kind, obj)) => {
            let s = crd::summary(cluster, &kind, &obj);
            if let Err(e) = st.store.upsert_job(&s).await {
                warn!(cluster = %cluster, job = %name, error = %e, "index refresh failed");
            }
            st.mark_index_fresh();
        }
        Err(e) => warn!(cluster = %cluster, job = %name, error = %e, "index refresh skipped"),
    }
}

/// 去掉服务端生成、由客户端带进来会出错的字段。
fn strip_server_fields(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("status");
        if let Some(meta) = obj.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            for k in [
                "managedFields",
                "uid",
                "creationTimestamp",
                "generation",
                "selfLink",
                "resourceVersion",
            ] {
                meta.remove(k);
            }
            if let Some(ann) = meta.get_mut("annotations").and_then(|a| a.as_object_mut()) {
                ann.remove("kubectl.kubernetes.io/last-applied-configuration");
            }
        }
    }
}

/// 把误落在 `spec.metadata` 的 labels/annotations 搬回顶层 `metadata`。
///
/// `FlinkDeployment` CRD 没有 `spec.metadata`，apiserver 会静默裁剪，
/// 于是"向导里填的标签想加到 JobManager 上"变成静默丢失；
/// 更糟的是这类非法字段能通过平台自己的校验与 dryRun。
fn hoist_spec_metadata(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let stray = obj
        .get_mut("spec")
        .and_then(|s| s.as_object_mut())
        .and_then(|s| s.remove("metadata"));
    let Some(stray) = stray else { return };
    for field in ["labels", "annotations"] {
        let Some(src) = stray.get(field).and_then(|v| v.as_object()) else {
            continue;
        };
        let mut merged = obj
            .get("metadata")
            .and_then(|m| m.get(field))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
        obj.entry("metadata")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|m| m.insert(field.to_string(), Value::Object(merged)));
    }
}

/// 读-改-写一律带 `metadata.resourceVersion` 前置条件。
///
/// 平台大量使用 merge patch（扩容/换镜像/恢复/开关配置），merge patch 本身不带
/// CAS：两个窗口同时改同一字段时后写会静默覆盖前写，接口却都返回 200。
/// 这里统一"先读 RV 再带条件写"：冲突由 apiserver 判 409，前端能看到"已被他人修改"。
async fn patch_cas(
    c: &flinkops_k8s::Cluster,
    kind: &str,
    ns: &str,
    name: &str,
    patch: Value,
) -> ApiResult<()> {
    let obj = c.get_raw(kind, ns, name).await?;
    let rv = crd::s(&obj, &["metadata", "resourceVersion"]).unwrap_or_default();
    c.patch_merge_cas(kind, ns, name, &rv, patch).await?;
    Ok(())
}

/// 400 响应带上 findings：前端据此逐条定位，而不是只看到一句话。
fn bad_request_with_findings(msg: &str, findings: &[flinkops_types::Finding]) -> Response {
    let codes: Vec<String> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| format!("{}@{}: {}", f.code, f.path, f.message))
        .collect();
    let body = json!({
        "error": {
            "code": "bad_request",
            "message": format!("{msg}：{}", codes.join("; ")),
        },
        "findings": findings,
    });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

// ---------------------------------------------------------------- 事件 / 异常

#[cfg(test)]
mod p1_tests {
    use super::*;

    #[test]
    fn savepoint_path_is_normalized_not_rejected() {
        // Flink/operator 记录的目录天然不带尾斜杠，平台要接受并补 /
        assert_eq!(
            normalize_savepoint_path("hdfs://encd/user/flink/savepoint/x/savepoint-1-2").unwrap(),
            "hdfs://encd/user/flink/savepoint/x/savepoint-1-2/"
        );
        assert_eq!(
            normalize_savepoint_path("s3://bucket/sp/1/").unwrap(),
            "s3://bucket/sp/1/"
        );
        assert!(normalize_savepoint_path("").is_err());
        assert!(normalize_savepoint_path("relative/path").is_err());
    }

    #[test]
    fn spec_metadata_is_hoisted_to_top_level() {
        let mut v = json!({
            "metadata": { "name": "j", "namespace": "flink", "labels": { "a": "1" } },
            "spec": {
                "image": "x:1",
                "metadata": { "labels": { "team": "data" }, "annotations": { "k": "v" } }
            }
        });
        hoist_spec_metadata(&mut v);
        assert!(
            v.pointer("/spec/metadata").is_none(),
            "spec.metadata 必须被移除"
        );
        assert_eq!(
            v.pointer("/metadata/labels/team").and_then(|x| x.as_str()),
            Some("data")
        );
        assert_eq!(
            v.pointer("/metadata/labels/a").and_then(|x| x.as_str()),
            Some("1")
        );
        assert_eq!(
            v.pointer("/metadata/annotations/k")
                .and_then(|x| x.as_str()),
            Some("v")
        );
    }
}
