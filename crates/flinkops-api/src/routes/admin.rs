//! 运维动作：VACUUM、备份信息、集群凭据（脱敏）。
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiResult;
use crate::extract::AuthUser;
use crate::state::AppState;

/// POST /api/admin/vacuum —— SQLite 空间回收
pub async fn vacuum(State(st): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;
    let before = st.store.db_bytes();
    let done = st.store.vacuum().await?;
    let after = st.store.db_bytes();
    st.audit(&user, "admin.vacuum", "*", "", "", "ok", None, None);
    Ok(Json(json!({
        "ok": true,
        "vacuumed": done,
        "beforeBytes": before,
        "afterBytes": after,
        "note": if done { "已执行 VACUUM" } else { "当前存储后端不是 SQLite，已跳过" },
    })))
}

/// GET /api/admin/backup —— 备份信息（SQLite 给文件路径，远程库给提示）
pub async fn backup(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    match st.store.backup_info() {
        Some((path, size)) => Ok(Json(json!({
            "kind": "sqlite",
            "path": path,
            "sizeBytes": size,
            "hint": "在宿主机上直接拷贝这个文件即可（WAL 模式下建议先停服务或用 sqlite3 .backup）",
        }))),
        None => Ok(Json(json!({
            "kind": "remote",
            "hint": "远程数据库请用 pg_dump / mysqldump 备份；本平台不托管备份",
        }))),
    }
}

/// GET /api/clusters/{id}/credential —— 集群接入信息（**不返回任何私钥**）
pub async fn credential(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let c = st.cluster(&id).await?;
    let jobs = c.list_jobs(None).await.map(|v| v.len()).unwrap_or(0);
    Ok(Json(json!({
        "cluster": id,
        "inCluster": c.in_cluster,
        "apiServer": c.api_host,
        "readonly": c.readonly,
        "jobCount": jobs,
        "note": "平台只保存 kubeconfig 路径（或 in-cluster 环境），不会外传证书与私钥",
    })))
}
/// GET /api/admin/diagnostic —— 一键诊断包（JSON，可直接发给别人排障）
///
/// 原则：**不导出任何凭据**。DSN 只保留 scheme，kubeconfig 只给 api-server host。
pub async fn diagnostic(State(st): State<Arc<AppState>>, u: AuthUser) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;

    let store_kind = if st.cfg.store_dsn.starts_with("sqlite") {
        "sqlite"
    } else if st.cfg.store_dsn.starts_with("postgres") {
        "postgres"
    } else if st.cfg.store_dsn.starts_with("mysql") {
        "mysql"
    } else {
        "unknown"
    };

    let mut clusters = Vec::new();
    for (id, c) in &st.cluster_entries().await {
        let kinds = c.supported_kinds().await.unwrap_or_default();
        let jobs = c.list_jobs(None).await.map(|v| v.len()).unwrap_or(0);
        clusters.push(json!({
            "id": id,
            "inCluster": c.in_cluster,
            "apiServer": c.api_host,
            "readonly": c.readonly,
            "supportedKinds": kinds,
            "jobCount": jobs,
        }));
    }

    let jobs = st.store.list_jobs(None).await?;
    let by_health = json!({
        "green": jobs.iter().filter(|j| j.health_level() == "green").count(),
        "yellow": jobs.iter().filter(|j| j.health_level() == "yellow").count(),
        "red": jobs.iter().filter(|j| j.health_level() == "red").count(),
    });
    let running = jobs
        .iter()
        .filter(|j| j.state.eq_ignore_ascii_case("RUNNING"))
        .count();

    let rules = st.store.list_rules().await.unwrap_or_default();
    let open_events = st.store.list_events(Some("OPEN")).await.unwrap_or_default();
    let audit = st.store.list_audit(20).await.unwrap_or_default();

    let prom = match st.prometheus_base().await {
        Ok(base) => {
            json!({ "configured": st.cfg.prometheus_url.clone(), "reachable": true, "base": base })
        }
        Err(_) => json!({ "configured": st.cfg.prometheus_url.clone(), "reachable": false }),
    };

    Ok(Json(json!({
        "generatedAt": flinkops_types::now_rfc3339(),
        "platform": {
            "version": env!("CARGO_PKG_VERSION"),
            "bind": st.cfg.bind.to_string(),
            "webDir": st.cfg.web_dir,
            "readonly": st.cfg.readonly,
            "auditRetentionDays": st.cfg.audit_retention_days,
            "dbMaxMb": st.cfg.db_max_mb,
        },
        "store": { "kind": store_kind, "bytes": st.store.db_bytes() },
        "prometheus": prom,
        "clusters": clusters,
        "jobs": { "total": jobs.len(), "running": running, "byHealth": by_health },
        "alerts": { "rules": rules.len(), "openEvents": open_events.len() },
        "recentAudit": audit,
        "note": "不包含 kubeconfig、私钥、密码、DSN 细节；可安全分享",
    })))
}
/// GET /api/admin/crd-schema/{crdName} —— 打印 CRD 的 spec 字段（排字段名用）
pub async fn crd_schema(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(crd_name): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let cluster_id = st
        .cluster_ids()
        .await
        .into_iter()
        .next()
        .unwrap_or_default();
    let (keys, required, detail) = st
        .cluster(&cluster_id)
        .await?
        .crd_spec_schema(&crd_name)
        .await?;
    Ok(Json(json!({
        "crd": crd_name,
        "specKeys": keys,
        "required": required,
        "specSchema": detail,
    })))
}
