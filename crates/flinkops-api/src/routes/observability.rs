//! 监控接入的"下发"能力：PodMonitor 落地 + 一键补齐标签。
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;

use axum::Json;
use flinkops_types::{crd, Error};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodMonitorBody {
    pub yaml: String,
    pub cluster: Option<String>,
}

/// POST /api/observability/podmonitor —— 把前端生成/编辑的 PodMonitor apply 到集群
pub async fn apply_podmonitor(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Json(body): Json<PodMonitorBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    let obj: Value = serde_yaml::from_str(&body.yaml)
        .map_err(|e| ApiError(Error::BadRequest(format!("YAML 解析失败: {e}"))))?;

    let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or_default();
    if kind != "PodMonitor" {
        return Err(ApiError(Error::BadRequest(format!(
            "只接受 PodMonitor，收到 {kind}"
        ))));
    }
    let ns = obj
        .get("metadata")
        .and_then(|m| m.get("namespace"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError(Error::BadRequest("metadata.namespace 不能为空".into())))?
        .to_string();

    let cluster_id = match body.cluster.clone() {
        Some(c) => c,
        None if st.clusters_count().await == 1 => st
            .cluster_ids()
            .await
            .into_iter()
            .next()
            .unwrap_or_default(),
        None => {
            return Err(ApiError(Error::BadRequest(
                "已接入多个集群，请用 cluster 指定目标".into(),
            )))
        }
    };

    let c = st.cluster(&cluster_id).await?;
    if !c.namespace_exists(&ns).await? {
        return Err(ApiError(Error::BadRequest(format!("命名空间 {ns} 不存在"))));
    }
    let name = c
        .apply_custom("monitoring.coreos.com", "v1", "PodMonitor", &ns, obj)
        .await?;

    st.audit(
        &user,
        "observability.podmonitor",
        &name,
        &cluster_id,
        &ns,
        "ok",
        Some("PodMonitor applied".into()),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "cluster": cluster_id,
        "namespace": ns,
        "name": name,
        "hint": "30–60s 后到 Prometheus 的 Status→Targets 看 flink target 是否 up",
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsBody {
    /// 要打的标签；默认 flinkops.io/scrape=true
    pub labels: Option<BTreeMap<String, String>>,
    /// 只处理 NodePort 暴露的作业（默认 true，安全项优先）
    pub only_nodeport: Option<bool>,
    pub cluster: Option<String>,
}

/// POST /api/observability/patch-labels —— 给作业补 metadata.labels
///
/// 只改 label，不碰 spec：不会触发重新部署，也不会影响运行中的作业。
pub async fn patch_labels(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Json(body): Json<LabelsBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    let labels = body.labels.unwrap_or_else(|| {
        BTreeMap::from([("flinkops.io/scrape".to_string(), "true".to_string())])
    });
    let only_np = body.only_nodeport.unwrap_or(true);

    let jobs = st.store.list_jobs(body.cluster.as_deref()).await?;
    let targets: Vec<_> = jobs
        .iter()
        .filter(|j| !only_np || j.rest_exposed.as_deref() == Some("NodePort"))
        .collect();

    let mut ok = 0usize;
    let mut failed = 0usize;
    for j in &targets {
        let patch = json!({ "metadata": { "labels": labels } });
        match st.cluster(&j.cluster).await {
            Ok(c) => match c
                .patch_merge(crd::KIND_DEPLOYMENT, &j.namespace, &j.name, patch)
                .await
            {
                Ok(_) => ok += 1,
                Err(_) => failed += 1,
            },
            Err(_) => failed += 1,
        }
    }

    st.audit(
        &user,
        "observability.patch-labels",
        "*",
        body.cluster.as_deref().unwrap_or(""),
        "",
        "ok",
        Some(format!("ok={ok} failed={failed}")),
        None,
    );
    Ok(Json(
        json!({ "ok": true, "patched": ok, "failed": failed, "total": targets.len() }),
    ))
}
