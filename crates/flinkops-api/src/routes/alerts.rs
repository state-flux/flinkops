//! 告警规则与事件。
//!
//! 规则类型刻意只做"用现有数据就能算"的四种，不假装支持任意 PromQL：
//! - job_not_running：state != RUNNING
//! - restarts_high：restartNonce > threshold
//! - nodeport_exposed：REST 用 NodePort 暴露（安全项）
//! - slots_over：并行度 > 槽位总量
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::Json;
use flinkops_types::{now_rfc3339, Error};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;

use crate::state::AppState;

pub const KINDS: [&str; 4] = [
    "job_not_running",
    "restarts_high",
    "nodeport_exposed",
    "slots_over",
];

pub async fn list_rules(State(st): State<Arc<AppState>>, u: AuthUser) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items = st.store.list_rules().await?;
    Ok(Json(json!({ "items": items, "kinds": KINDS })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleBody {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub threshold: Option<i64>,
    pub severity: Option<String>,
    pub enabled: Option<bool>,
}

pub async fn save_rule(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<RuleBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if !KINDS.contains(&body.kind.as_str()) {
        return Err(ApiError(Error::BadRequest(format!(
            "kind 只能是 {}",
            KINDS.join(" / ")
        ))));
    }
    let id = body.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let rule = json!({
        "id": id,
        "name": body.name,
        "kind": body.kind,
        "threshold": body.threshold,
        "severity": body.severity.unwrap_or_else(|| "warn".into()),
        "enabled": body.enabled.unwrap_or(true),
    });
    st.store.save_rule(&rule).await?;
    st.audit(&user, "alert.rule.save", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true, "rule": rule })))
}

pub async fn delete_rule(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.delete_rule(&id).await?;
    st.audit(&user, "alert.rule.delete", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_events(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items = st
        .store
        .list_events(q.get("state").map(String::as_str))
        .await?;
    let open = items.iter().filter(|e| e["state"] == "OPEN").count();
    Ok(Json(json!({ "items": items, "open": open })))
}

pub async fn ack_event(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.ack_event(&id).await?;
    st.audit(&user, "alert.ack", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}

/// "NodePort 暴露"是否算问题：**只有平台以 in-cluster 身份接入时才成立**。
///
/// 平台在集群外时 ClusterIP 根本连不上（反代实测 502），NodePort 是唯一可行的路，
/// 这时报"建议改 ClusterIP"只会制造噪音。
pub fn nodeport_alert_applies(rest_exposed: Option<&str>, in_cluster: bool) -> bool {
    in_cluster && rest_exposed == Some("NodePort")
}

/// 求值：读作业索引 + 规则，产出/刷新事件。由后台循环每 60s 调用一次。
pub async fn evaluate(st: &Arc<AppState>) -> usize {
    let rules = match st.store.list_rules().await {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let enabled: Vec<&Value> = rules.iter().filter(|r| r["enabled"] == true).collect();
    if enabled.is_empty() {
        return 0;
    }
    let jobs = st.store.list_jobs(None).await.unwrap_or_default();
    let mut n = 0;

    // 平台是以 in-cluster 身份接入的吗？只有 in-cluster 时"NodePort 暴露"才是**可优化项**
    // （改 ClusterIP 后反代仍可打开 WebUI）；平台在集群外时 NodePort 是唯一可行的路，
    // 报这条只会制造噪音，反代根本连不到 ClusterIP（实测 502）。
    let mut in_cluster: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for cid in st.cluster_ids().await {
        if let Ok(c) = st.cluster(&cid).await {
            in_cluster.insert(cid, c.in_cluster);
        }
    }

    for r in enabled {
        let kind = r["kind"].as_str().unwrap_or_default();
        let rule_id = r["id"].as_str().unwrap_or_default().to_string();
        let threshold = r["threshold"].as_i64().unwrap_or(0);
        let severity = r["severity"].as_str().unwrap_or("warn");

        // 不适用的集群：NodePort 建议不成立，顺手把历史 OPEN 事件关掉
        let suppressed: Vec<String> = if kind == "nodeport_exposed" {
            in_cluster
                .iter()
                .filter(|(_, v)| !**v)
                .map(|(k, _)| k.clone())
                .collect()
        } else {
            Vec::new()
        };

        // 本轮命中的 (cluster, job)：其余 OPEN 事件视为已恢复，收尾统一关闭
        let mut hits: Vec<(String, String)> = Vec::new();

        for j in &jobs {
            let hit = match kind {
                "job_not_running" => !j.state.eq_ignore_ascii_case("RUNNING"),
                "restarts_high" => j.restart_nonce.unwrap_or(0) > threshold,
                "nodeport_exposed" => nodeport_alert_applies(
                    j.rest_exposed.as_deref(),
                    *in_cluster.get(&j.cluster).unwrap_or(&false),
                ),
                "slots_over" => {
                    let total =
                        (j.task_manager_replicas.unwrap_or(0)) * (j.slots_per_tm.unwrap_or(0));
                    total > 0 && j.parallelism.unwrap_or(0) > total
                }
                _ => false,
            };
            if !hit {
                continue;
            }
            let msg = match kind {
                "job_not_running" => format!("{} 当前状态 {}", j.name, j.state),
                "restarts_high" => format!("{} restartNonce={} 超过阈值 {}", j.name, j.restart_nonce.unwrap_or(0), threshold),
                "nodeport_exposed" => format!(
                    "{} 的 REST 用 NodePort 暴露；平台已在集群内（in-cluster），可改 ClusterIP + 反代收敛入口",
                    j.name
                ),
                _ => format!(
                    "{} 并行度 {} 超过槽位总量 {}×{}",
                    j.name,
                    j.parallelism.unwrap_or(0),
                    j.task_manager_replicas.unwrap_or(0),
                    j.slots_per_tm.unwrap_or(0)
                ),
            };
            let e = json!({
                "ruleId": r["id"], "ruleName": r["name"], "kind": kind,
                "cluster": j.cluster, "namespace": j.namespace, "job": j.name,
                "severity": severity, "message": msg,
            });
            if st.store.upsert_event(&e).await.is_ok() {
                n += 1;
                hits.push((j.cluster.clone(), j.name.clone()));
            }
        }

        // 告警自愈：本轮没命中的（作业已恢复 / 已不适用）一律 CLOSED
        if let Ok(closed) = st.store.close_stale_events(&rule_id, &hits).await {
            if closed > 0 {
                tracing::debug!(rule = %rule_id, closed, "stale alert events closed");
            }
        }
        for c in &suppressed {
            let _ = st.store.close_events(&rule_id, Some(c)).await;
        }
    }
    let _ = now_rfc3339();
    n
}

#[cfg(test)]
mod tests {
    use super::nodeport_alert_applies;

    #[test]
    fn nodeport_alert_only_when_platform_is_in_cluster() {
        // 平台在集群内：NodePort 是可优化项 → 报
        assert!(nodeport_alert_applies(Some("NodePort"), true));
        // 平台在集群外：NodePort 是唯一的路 → 不报
        assert!(!nodeport_alert_applies(Some("NodePort"), false));
        // 本来就是 ClusterIP / 没有 REST：两种情况都不报
        assert!(!nodeport_alert_applies(Some("ClusterIP"), true));
        assert!(!nodeport_alert_applies(None, true));
    }
}
