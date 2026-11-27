//! FlinkSessionJob：把作业提交到已有 session cluster 的官方方式。
//!
//! 相比「经 SQL Gateway 提交」，SessionJob 是一个独立的 CRD：
//! - 有自己的生命周期（停作业不影响集群）
//! - jobId / 状态由 operator 写进 status（不用猜）
//! - 支持 upgradeMode（stateless / savepoint / last-state）
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::Json;
use flinkops_types::{crd, Error};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub name: String,
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    /// 目标 session cluster（FlinkDeployment 名）
    pub deployment_name: String,
    pub jar_uri: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub parallelism: Option<i64>,
    /// stateless | savepoint | last-state
    pub upgrade_mode: Option<String>,
    pub entry_class: Option<String>,
    pub initial_savepoint_path: Option<String>,
}

/// GET /api/session-jobs —— 列出所有 FlinkSessionJob
pub async fn list(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let cluster = q.get("cluster").cloned();
    let tok = crate::mcp_guard::peek(&st, &headers).await?;
    let ids = st.cluster_ids().await;
    let targets: Vec<String> = match cluster {
        Some(c) => vec![c],
        None => ids,
    };

    let mut out = Vec::new();
    for cid in targets {
        let c = match st.cluster(&cid).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let items = match c.list_raw(crd::KIND_SESSION_JOB, None).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for o in items {
            let meta = o.get("metadata").cloned().unwrap_or(Value::Null);
            let job = o.pointer("/spec/job").cloned().unwrap_or(Value::Null);
            if let Some(t) = &tok {
                let ns = meta.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
                let n = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if !crate::mcp_guard::job_allowed(t, ns, n) {
                    continue;
                }
            }
            out.push(json!({
                "cluster": cid,
                "namespace": meta.get("namespace").and_then(|v| v.as_str()).unwrap_or(""),
                "name": meta.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "deploymentName": o.pointer("/spec/deploymentName").and_then(|v| v.as_str()).unwrap_or(""),
                "jarUri": job.get("jarURI").and_then(|v| v.as_str()),
                "args": job.get("args").cloned().unwrap_or(json!([])),
                "parallelism": job.get("parallelism").and_then(|v| v.as_i64()),
                "upgradeMode": job.get("upgradeMode").and_then(|v| v.as_str()),
                "state": o.pointer("/status/jobStatus/state").and_then(|v| v.as_str()),
                "jobId": o.pointer("/status/jobStatus/jobId").and_then(|v| v.as_str()),
                "error": o.pointer("/status/error").and_then(|v| v.as_str()),
            }));
        }
    }
    Ok(Json(json!({ "items": out })))
}

/// POST /api/session-jobs —— 创建/更新 FlinkSessionJob
pub async fn create(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    if body.name.trim().is_empty() || body.jar_uri.trim().is_empty() {
        return Err(ApiError(Error::BadRequest(
            "name 与 jarUri 都不能为空".into(),
        )));
    }
    let cluster_id = body.cluster.clone().unwrap_or_else(|| "local".into());
    let ns = body.namespace.clone().unwrap_or_else(|| "flink".into());
    // SessionJob 也是往集群里塞作业（可指定任意 jarURI），与提交作业同等对待
    crate::mcp_guard::gate(
        &st,
        &headers,
        "flinkops_create_job",
        &ns,
        body.name.trim(),
        serde_json::json!({ "jarURI": body.jar_uri }),
        true,
    )
    .await
    .map_err(ApiError)?;
    let c = st.cluster(&cluster_id).await?;

    // 目标会话集群必须存在且是 session 模式（没有 job 段）
    let target = c
        .get_raw(crd::KIND_DEPLOYMENT, &ns, &body.deployment_name)
        .await?;
    if target.get("spec").and_then(|s| s.get("job")).is_some() {
        return Err(ApiError(Error::BadRequest(format!(
            "{} 是 application 模式（有 job 段），SessionJob 只能提交到 session cluster",
            body.deployment_name
        ))));
    }

    let mode = body
        .upgrade_mode
        .clone()
        .unwrap_or_else(|| "stateless".into());
    if !["stateless", "savepoint", "last-state"].contains(&mode.as_str()) {
        return Err(ApiError(Error::BadRequest(
            "upgradeMode 只能是 stateless / savepoint / last-state".into(),
        )));
    }

    let mut job = json!({
        "jarURI": body.jar_uri.trim(),
        "parallelism": body.parallelism.unwrap_or(1),
        "upgradeMode": mode,
    });
    if !body.args.is_empty() {
        job["args"] = json!(body.args);
    }
    if let Some(ec) = &body.entry_class {
        job["entryClass"] = json!(ec);
    }
    if let Some(sp) = &body.initial_savepoint_path {
        job["initialSavepointPath"] = json!(sp);
    }

    let obj = json!({
        "apiVersion": "flink.apache.org/v1beta1",
        "kind": crd::KIND_SESSION_JOB,
        "metadata": {
            "name": body.name.trim(),
            "namespace": ns,
            "labels": { "flinkops.io/managed-by": "flinkops" }
        },
        "spec": {
            "deploymentName": body.deployment_name,
            "job": job,
        }
    });

    c.apply_custom(
        "flink.apache.org",
        "v1beta1",
        crd::KIND_SESSION_JOB,
        &ns,
        obj,
    )
    .await?;
    st.audit(
        &user,
        "sessionjob.create",
        body.name.trim(),
        &cluster_id,
        &ns,
        "ok",
        Some(format!("deployment={}", body.deployment_name)),
        None,
    );
    Ok(Json(
        json!({ "ok": true, "name": body.name.trim(), "deploymentName": body.deployment_name }),
    ))
}

/// DELETE /api/session-jobs/{cluster}/{ns}/{name}
pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path((cluster, ns, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    crate::mcp_guard::gate(
        &st,
        &headers,
        "flinkops_delete_job",
        &ns,
        &name,
        serde_json::json!({ "cluster": cluster }),
        true,
    )
    .await
    .map_err(ApiError)?;
    st.cluster(&cluster)
        .await?
        .delete_object(crd::KIND_SESSION_JOB, &ns, &name)
        .await?;
    st.audit(
        &user,
        "sessionjob.delete",
        &name,
        &cluster,
        &ns,
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "deleted": name })))
}
