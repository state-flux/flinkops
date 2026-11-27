//! 集群能力探测：operator 实际版本 + 支持哪些 Flink CRD。
//!
//! 升级 operator 后，"作业 status 还是旧格式"很常见（存量 CR 不会自动重写），
//! 所以这里同时给出 `withoutObservedGeneration` 计数，用于判断是否需要触发一次 reconcile。
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn info(State(st): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let _ = st.require_user(&headers).await?;

    let mut clusters = Vec::new();
    for (id, c) in &st.cluster_entries().await {
        let kinds = c.supported_kinds().await.unwrap_or_default().len();
        let operators = c.find_operator().await.unwrap_or_default();
        let kind_list = c.supported_kinds().await.unwrap_or_default();
        clusters.push(json!({
            "id": id,
            "inCluster": c.in_cluster,
            "supportedKinds": kind_list,
            "kindCount": kinds,
            "operators": operators,
        }));
    }

    let jobs = st.store.list_jobs(None).await?;
    let stale = jobs
        .iter()
        .filter(|j| j.observed_generation.is_none())
        .count();
    let has_conditions = jobs.iter().filter(|j| !j.conditions.is_empty()).count();

    Ok(Json(json!({
        "clusters": clusters,
        "jobs": {
            "total": jobs.len(),
            "withConditions": has_conditions,
            "withoutObservedGeneration": stale,
            "note": if stale > 0 {
                "存量作业的 status 仍是旧 operator 写的；改一次 spec（或 bump restartNonce）触发 reconcile 后就会写 conditions / observedGeneration"
            } else {
                "作业 status 已由新 operator 重写"
            },
        }
    })))
}
