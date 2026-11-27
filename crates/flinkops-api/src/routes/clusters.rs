//! 多集群动态接入与重连。
//!
//! 重连 = 按**原始接入方式**（in-cluster / kubeconfig 路径）重建 client，
//! 不是为了"修连接"，而是让改了 kubeconfig、或 operator 重装后能刷新句柄。
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_k8s::Cluster;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::ClusterSource;
use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};
use crate::state::AppState;

/// 粘贴 kubeconfig 的体积上限：正常 kubeconfig 只有几 KB，超过这个数基本是贴错了东西
const MAX_KUBECONFIG_BYTES: usize = 512 * 1024;

/// 把粘贴进来的 kubeconfig 落盘（目录 0700、文件 0600）。
///
/// 为什么不直接存进数据库：kubeconfig 里含 client key，属于"落盘可以、进库不合适"的
/// 一类凭据 —— 文件系统权限能兜一层，也方便运维直接用 kubectl 复用同一份。
fn persist_inline_kubeconfig(
    dir: &std::path::Path,
    id: &str,
    content: &str,
) -> Result<std::path::PathBuf, Error> {
    if content.trim().is_empty() {
        return Err(Error::BadRequest("kubeconfig 内容为空".into()));
    }
    if content.len() > MAX_KUBECONFIG_BYTES {
        return Err(Error::BadRequest(format!(
            "kubeconfig 太大（{} KB），上限 {} KB",
            content.len() / 1024,
            MAX_KUBECONFIG_BYTES / 1024
        )));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Internal(format!("创建 {} 失败：{e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(format!("{id}.kubeconfig"));
    std::fs::write(&path, content)
        .map_err(|e| Error::Internal(format!("写入 {} 失败：{e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// 把 body 里的接入信息解析成 (来源, 是否临时落盘文件)——粘贴模式会写文件。
async fn resolve_source(
    st: &AppState,
    id: &str,
    kubeconfig_path: Option<&str>,
    kubeconfig: Option<&str>,
) -> Result<(ClusterSource, Option<std::path::PathBuf>), Error> {
    if let Some(text) = kubeconfig.map(str::trim).filter(|s| !s.is_empty()) {
        // 先校验 YAML 能不能用，避免把垃圾写进盘（解析逻辑在 flinkops-k8s 里）
        if let Err(e) = Cluster::from_kubeconfig_text("probe", text, "粘贴内容", true).await {
            return Err(Error::BadRequest(format!("kubeconfig 无效：{e}")));
        }
        let path = persist_inline_kubeconfig(&st.cfg.clusters_dir, id, text)?;
        return Ok((
            ClusterSource::Kubeconfig(path.display().to_string()),
            Some(path),
        ));
    }
    match kubeconfig_path.map(str::trim) {
        Some(p) if !p.is_empty() => {
            if !std::path::Path::new(p).exists() {
                return Err(Error::BadRequest(format!("kubeconfig 不存在：{p}")));
            }
            Ok((ClusterSource::Kubeconfig(p.to_string()), None))
        }
        _ => Ok((ClusterSource::InCluster, None)),
    }
}

async fn build(
    id: &str,
    source: &ClusterSource,
    readonly: bool,
) -> flinkops_types::Result<Cluster> {
    match source {
        ClusterSource::InCluster => Cluster::try_default(id.to_string(), readonly).await,
        ClusterSource::Kubeconfig(path) => {
            Cluster::from_kubeconfig(id.to_string(), path, readonly).await
        }
    }
}

/// POST /api/clusters/{id}/reconnect
pub async fn reconnect(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    let _ = st.cluster(&id).await?;
    let source = st
        .cluster_sources
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError(Error::BadRequest(format!("集群 {id} 没有保存接入来源"))))?;

    let fresh = build(&id, &source, st.cfg.readonly).await.map_err(|e| {
        ApiError(Error::Upstream {
            target: format!("cluster {id}"),
            message: format!("重连失败：{e}"),
        })
    })?;
    let jobs = fresh.list_jobs(None).await.map(|v| v.len()).unwrap_or(0);

    st.clusters
        .write()
        .await
        .insert(id.clone(), Arc::new(fresh));
    // 端点（nodePort 等）可能变了，清掉 REST 缓存
    st.rest_cache.write().await.clear();

    st.audit(
        &user,
        "cluster.reconnect",
        &id,
        &id,
        "",
        "ok",
        Some(format!("jobs={jobs}")),
        None,
    );
    Ok(Json(json!({ "ok": true, "cluster": id, "jobCount": jobs })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddBody {
    pub id: String,
    /// 留空表示 in-cluster
    pub kubeconfig_path: Option<String>,
    /// 直接粘贴 kubeconfig 内容（与 kubeconfigPath 二选一，优先这个）
    pub kubeconfig: Option<String>,
    #[serde(default)]
    pub readonly: bool,
}

/// POST /api/clusters —— 动态接入一个集群（不必重启）
pub async fn add(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<AddBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let id = body.id.trim().to_string();
    if id.is_empty() || id.contains('/') {
        return Err(ApiError(Error::BadRequest(
            "集群 id 不能为空且不能含 /".into(),
        )));
    }
    if st.clusters.read().await.contains_key(&id) {
        return Err(ApiError(Error::Conflict(format!("集群 {id} 已接入"))));
    }

    let (source, inline_file) = resolve_source(
        &st,
        &id,
        body.kubeconfig_path.as_deref(),
        body.kubeconfig.as_deref(),
    )
    .await
    .map_err(ApiError)?;

    let fresh = match build(&id, &source, body.readonly).await {
        Ok(c) => c,
        Err(e) => {
            // 连接失败就把刚落盘的 kubeconfig 删掉，别留下孤儿凭据
            if let Some(f) = &inline_file {
                let _ = std::fs::remove_file(f);
            }
            return Err(ApiError(Error::Upstream {
                target: format!("cluster {id}"),
                message: format!("连接失败：{e}"),
            }));
        }
    };
    // 探活：能列作业才算真的接上
    let kinds = fresh.supported_kinds().await.unwrap_or_default();
    let jobs = fresh.list_jobs(None).await.map(|v| v.len()).unwrap_or(0);

    st.clusters
        .write()
        .await
        .insert(id.clone(), Arc::new(fresh));
    st.cluster_sources
        .write()
        .await
        .insert(id.clone(), source.clone());
    st.rest_cache.write().await.clear();

    // 持久化：重启后仍在（env 里配置的集群不会进库，避免重复）
    let (kind, path) = match &source {
        ClusterSource::InCluster => ("incluster", None),
        ClusterSource::Kubeconfig(p) => ("kubeconfig", Some(p.as_str())),
    };
    if let Err(e) = st.store.save_cluster(&id, kind, path, body.readonly).await {
        tracing::warn!(error = %e, "cluster persist failed (still usable in memory)");
    }

    // 立刻同步一次作业索引：否则切到新集群时列表是空的
    let synced = crate::routes::jobs::sync_clusters(&st, Some(&id), None, true)
        .await
        .unwrap_or(0);
    tracing::info!(cluster = %id, synced, "cluster synced on add");

    st.audit(
        &user,
        "cluster.add",
        &id,
        &id,
        "",
        "ok",
        Some(format!("jobs={jobs}")),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "cluster": id,
        "jobCount": jobs,
        "synced": synced,
        "supportedKinds": kinds,
    })))
}
/// DELETE /api/clusters/{id} —— 移除动态接入的集群（env 里的用重启移除）
pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    if st.cfg.clusters.iter().any(|(eid, _)| eid == &id) {
        return Err(ApiError(Error::BadRequest(format!(
            "集群 {id} 来自 FLINKOPS_CLUSTERS，请改配置后重启"
        ))));
    }
    // 粘贴进来的 kubeconfig 是本平台落盘的，移除集群时一并清掉
    if let Some(ClusterSource::Kubeconfig(path)) = st.cluster_sources.read().await.get(&id).cloned()
    {
        let p = std::path::Path::new(&path);
        if p.starts_with(&st.cfg.clusters_dir) {
            let _ = std::fs::remove_file(p);
        }
    }
    st.clusters.write().await.remove(&id);
    st.cluster_sources.write().await.remove(&id);
    st.rest_cache.write().await.clear();
    st.store.delete_cluster(&id).await?;
    // 顺带清掉它的作业索引，否则列表会留下"幽灵作业"
    let pruned = st.store.delete_jobs_by_cluster(&id).await.unwrap_or(0);
    if pruned > 0 {
        tracing::info!(cluster = %id, pruned, "cluster jobs index purged");
    }
    st.audit(&user, "cluster.remove", &id, &id, "", "ok", None, None);
    Ok(Json(json!({ "ok": true, "removed": id })))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadonlyBody {
    pub readonly: bool,
}

/// PATCH /api/clusters/{id} —— 切换只读（重建 client 并同步持久化）
pub async fn set_readonly(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<ReadonlyBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let source = st
        .cluster_sources
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError(Error::BadRequest(format!("集群 {id} 没有保存接入来源"))))?;

    let fresh = build(&id, &source, body.readonly).await.map_err(|e| {
        ApiError(Error::Upstream {
            target: format!("cluster {id}"),
            message: format!("重建失败：{e}"),
        })
    })?;
    st.clusters
        .write()
        .await
        .insert(id.clone(), Arc::new(fresh));
    st.rest_cache.write().await.clear();

    let (kind, path) = match &source {
        ClusterSource::InCluster => ("incluster", None),
        ClusterSource::Kubeconfig(p) => ("kubeconfig", Some(p.as_str())),
    };
    let persistent = !st.cfg.clusters.iter().any(|(eid, _)| eid == &id);
    if persistent {
        st.store
            .save_cluster(&id, kind, path, body.readonly)
            .await?;
    }

    st.audit(
        &user,
        "cluster.readonly",
        &id,
        &id,
        "",
        "ok",
        Some(format!("readonly={}", body.readonly)),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "cluster": id,
        "readonly": body.readonly,
        "persisted": persistent,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeBody {
    /// 留空表示測試 in-cluster
    pub kubeconfig_path: Option<String>,
    /// 直接粘贴 kubeconfig 内容
    pub kubeconfig: Option<String>,
}

/// POST /api/clusters/test —— 接入前测试能否连上 K8s（不落库、不改集群）
pub async fn test(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<ProbeBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let started = std::time::Instant::now();
    // 探活用的临时 client：id 固定，避免和已有集群重名
    let probe_id = "probe";
    let fresh = match (
        body.kubeconfig
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        body.kubeconfig_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(text), _) => {
            Cluster::from_kubeconfig_text(probe_id, text, "粘贴内容", st.cfg.readonly).await
        }
        (None, Some(path)) => Cluster::from_kubeconfig(probe_id, path, st.cfg.readonly).await,
        (None, None) => Cluster::try_default(probe_id, st.cfg.readonly).await,
    }
    .map_err(|e| ApiError(Error::BadRequest(format!("无法建立连接：{e}"))))?;

    let report = fresh.probe().await.map_err(|e| {
        ApiError(Error::Upstream {
            target: "kube-apiserver".into(),
            message: format!("连接失败：{e}"),
        })
    })?;
    let latency_ms = started.elapsed().as_millis() as u64;

    st.audit(
        &user,
        "cluster.test",
        probe_id,
        "",
        "",
        "ok",
        Some(format!(
            "version={} jobs={} latency={latency_ms}ms",
            report.server_version, report.job_count
        )),
        None,
    );

    Ok(Json(json!({
        "ok": true,
        "serverVersion": report.server_version,
        "jobCount": report.job_count,
        "supportedKinds": report.kinds,
        "latencyMs": latency_ms,
    })))
}
