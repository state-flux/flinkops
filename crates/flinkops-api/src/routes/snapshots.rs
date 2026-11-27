//! FlinkStateSnapshot 列表（保存点/检查点的 CR 视图）。
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::Json;
use flinkops_types::{crd, Error, SnapshotListItem};
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};

use crate::state::AppState;

pub async fn list(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let only_cluster = q.get("cluster").cloned();
    let only_ns = q.get("namespace").cloned();
    let only_state = q.get("state").cloned();

    let mut items: Vec<SnapshotListItem> = Vec::new();
    // 旧版 operator 没有 FlinkStateSnapshot CRD：这是"能力缺失"，不是错误，
    // 单独标出来让 UI 说清楚，而不是让整页 404。
    let mut unsupported: Vec<String> = Vec::new();

    for (id, c) in &st.cluster_entries().await {
        if let Some(want) = &only_cluster {
            if want != id {
                continue;
            }
        }
        let objects = match c.list_snapshots(only_ns.as_deref()).await {
            Ok(v) => v,
            Err(Error::NotFound(_)) => {
                unsupported.push(id.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        for obj in objects {
            items.push(SnapshotListItem {
                cluster: id.clone(),
                namespace: crd::s(&obj, &["metadata", "namespace"]).unwrap_or_default(),
                name: crd::s(&obj, &["metadata", "name"]).unwrap_or_default(),
                job_name: crd::s(&obj, &["spec", "jobReference", "name"]),
                // 老 operator 不写 status.state：不猜，标 UNKNOWN
                state: crd::s(&obj, &["status", "state"]).unwrap_or_else(|| "UNKNOWN".to_string()),
                path: crd::s(&obj, &["status", "savepointLocation"])
                    .or_else(|| crd::s(&obj, &["status", "path"])),
                created_at: crd::s(&obj, &["metadata", "creationTimestamp"]),
            });
        }
    }

    // 多租户：非 admin 只能看到白名单 namespace 内的快照（快照 path 就是数据落点）
    if let Some(p) = st.principal(&headers).await {
        if !p.is_admin() {
            items.retain(|i| p.ns_ok(&i.namespace));
        }
    }
    if let Some(want) = &only_state {
        items.retain(|i| i.state.eq_ignore_ascii_case(want));
    }
    // MCP Token 的作业白名单：快照 path 往往就是对象存储地址，越权枚举危害不小
    if let Some(tok) = crate::mcp_guard::peek(&st, &headers).await? {
        items.retain(|i| match i.job_name.as_deref() {
            Some(j) if !j.is_empty() => crate::mcp_guard::job_allowed(&tok, &i.namespace, j),
            _ => crate::mcp_guard::ns_allowed(&tok, &i.namespace),
        });
    }
    // 新的在前
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(Json(json!({
        "items": items,
        "count": items.len(),
        "unsupportedClusters": unsupported,
    })))
}
/// DELETE /api/snapshots/{cluster}/{ns}/{name} —— 删除一个 FlinkStateSnapshot
pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path((cluster, ns, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    if st.cfg.readonly {
        return Err(ApiError(flinkops_types::Error::Forbidden(
            "平台处于只读模式".into(),
        )));
    }
    // 删快照 = 删恢复点，不可逆：MCP Token 需要 admin scope + 一次性审批
    crate::mcp_guard::gate(
        &st,
        &headers,
        "flinkops_delete_snapshot",
        &ns,
        &name,
        serde_json::json!({ "cluster": cluster }),
        true,
    )
    .await
    .map_err(ApiError)?;
    st.cluster(&cluster)
        .await?
        .delete_object(crd::KIND_SNAPSHOT, &ns, &name)
        .await?;
    st.audit(
        &user,
        "snapshot.delete",
        &name,
        &cluster,
        &ns,
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "deleted": name })))
}
