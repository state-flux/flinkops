//! Prometheus 透传：只有它能给历史与跨作业聚合。
//!
//! 平台自己不存指标点，所以"历史曲线"必须落到这里；
//! 实时值仍然走 Flink REST（毫秒级），两者不要混用。
use std::sync::Arc;

use axum::extract::{Query, State};

use axum::Json;
use flinkops_types::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstantQuery {
    pub query: String,
    pub time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeQuery {
    pub query: String,
    /// Unix 秒（可选，缺省 end=now, start=now-15m）
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub step: Option<i64>,
}

/// PromQL 允许的指标名前缀 —— 与 `docs/mcp-spec.md §3.1` 的白名单保持一致。
///
/// 平台只读 Flink / K8s / 节点指标；不给出任意 PromQL 直通，否则登录用户就能查
/// Prometheus 里其它业务的数据（开源/多用户场景下是实打实的越权面）。
const ALLOWED_PREFIXES: &[&str] = &[
    "flink_",
    "kube_",
    "node_",
    "container_",
    "machine_",
    "process_",
    "prometheus_",
    "up",
];

/// 粗粒度 parser：跳过字符串字面量，逐个取标识符，按"后面跟什么"判断它是
/// 函数名（后跟 `(`）、标签名（后跟 `=`/`!`/`~`）还是指标名。
/// 指标名（含下划线且不是纯数字）必须在白名单前缀里。
fn guard_query(query: &str) -> Result<()> {
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0usize;
    let mut in_str: Option<char> = None;
    // 处在 `by (...)` / `without (...)` / `on (...)` 这类标签列表里的深度：
    // 里面的标识符是标签名（job_id、task_name…），不是指标名。
    let mut label_depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(quote) = in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == quote {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            in_str = Some(c);
            i += 1;
            continue;
        }
        if label_depth > 0 {
            if c == '(' {
                label_depth += 1;
            } else if c == ')' {
                label_depth -= 1;
            }
            i += 1;
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == ':' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == ':')
            {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let next = chars.get(j).copied().unwrap_or(' ');
            // 聚合/匹配关键字后面跟括号：括号里全是标签名，整段跳过
            let kw = matches!(
                token.to_ascii_lowercase().as_str(),
                "by" | "without" | "on" | "ignoring" | "group_left" | "group_right"
            );
            if kw && next == '(' {
                label_depth = 1;
                i = j + 1;
                continue;
            }
            let is_func = next == '(';
            let is_label = matches!(next, '=' | '!' | '~');
            let has_alpha = token.chars().any(|ch| ch.is_alphabetic());
            if !is_func && !is_label && has_alpha && token.contains('_') {
                let ok = ALLOWED_PREFIXES
                    .iter()
                    .any(|p| token.starts_with(p) || token == *p);
                if !ok {
                    return Err(Error::BadRequest(format!(
                        "PromQL 只允许白名单指标（flink_/kube_/node_ 等），收到 {token}"
                    )));
                }
            }
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// GET /api/metrics/query —— PromQL 瞬时查询
pub async fn query(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<InstantQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    guard_query(&q.query).map_err(ApiError)?;
    let base = st.prometheus_base().await?;
    let mut url = reqwest::Url::parse(&format!("{base}/api/v1/query"))
        .map_err(|e| ApiError(Error::Internal(format!("prom url: {e}"))))?;
    url.query_pairs_mut().append_pair("query", &q.query);
    if let Some(t) = &q.time {
        url.query_pairs_mut().append_pair("time", t);
    }
    Ok(Json(fetch(&st, url).await?))
}

/// GET /api/metrics/query_range —— 时间区间查询（历史曲线用）
pub async fn query_range(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    guard_query(&q.query).map_err(ApiError)?;
    let base = st.prometheus_base().await?;
    let end = q.end.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let start = q.start.unwrap_or(end - 900);
    let requested = q.step.unwrap_or(15).max(1);

    // Prometheus 单次最多 11000 点；按 10k 留余量，向上取整，
    // 否则 7d + step=1 会被 Prometheus 以 "exceeded maximum resolution" 拒掉，
    // 前端只看到一个 502 / "没有数据"。
    let span = (end - start).max(1);
    let min_step = ((span + 9_999) / 10_000).max(1);
    let (step, adjusted) = if requested < min_step {
        (min_step, true)
    } else {
        (requested, false)
    };

    let mut url = reqwest::Url::parse(&format!("{base}/api/v1/query_range"))
        .map_err(|e| ApiError(Error::Internal(format!("prom url: {e}"))))?;
    url.query_pairs_mut()
        .append_pair("query", &q.query)
        .append_pair("start", &start.to_string())
        .append_pair("end", &end.to_string())
        .append_pair("step", &step.to_string());
    let mut body = fetch(&st, url).await?;
    if adjusted {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stepAdjusted".into(), json!(step));
        }
    }
    Ok(Json(body))
}

/// GET /api/prometheus/discover —— 诊断：列出集群里找到的 Prometheus 候选
pub async fn discover(State(st): State<Arc<AppState>>, u: AuthUser) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let mut found = Vec::new();
    for (id, c) in &st.cluster_entries().await {
        let cands = c.find_prometheus().await.unwrap_or_default();
        for cand in cands {
            found.push(json!({
                "cluster": id,
                "namespace": cand.namespace,
                "name": cand.name,
                "port": cand.port,
                "nodePort": cand.node_port,
                "serviceType": cand.service_type,
                "baseUrl": c.prom_base_url(&cand),
            }));
        }
    }
    Ok(Json(json!({
        "configured": st.cfg.prometheus_url.clone(),
        "items": found,
        "count": found.len(),
    })))
}

async fn fetch(st: &Arc<AppState>, url: reqwest::Url) -> Result<Value> {
    let resp = st
        .http
        .get(url.clone())
        .send()
        .await
        .map_err(|e| Error::Upstream {
            target: url.to_string(),
            message: e.to_string(),
        })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Upstream {
            target: url.to_string(),
            message: format!("{status}: {}", body.chars().take(300).collect::<String>()),
        });
    }
    resp.json::<Value>().await.map_err(|e| Error::Upstream {
        target: url.to_string(),
        message: format!("decode json: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::guard_query;

    #[test]
    fn allows_whitelisted_and_rejects_others() {
        assert!(
            guard_query(r#"sum(flink_taskmanager_job_task_operator_numRecordsInPerSecond)"#)
                .is_ok()
        );
        assert!(
            guard_query(r#"100 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))*100"#).is_ok()
        );
        assert!(guard_query(r#"count({__name__=~"flink_.+"}) > 0"#).is_ok());
        assert!(guard_query(r#"up{job="x"}"#).is_ok());
        // 标签值里的下划线不能被当成指标名
        assert!(guard_query(r#"flink_x{job="a_b_c"}"#).is_ok());
        // by (...)/without (...) 里的分组标签不能被当成指标名
        assert!(guard_query(r#"sum by (job_id, job_name) (flink_x)"#).is_ok());
        assert!(guard_query(r#"sum without (task_name) (flink_x)"#).is_ok());
        assert!(guard_query(r#"count(sum by (job_id) (flink_x) > 0) or vector(0)"#).is_ok());
        // 非白名单指标
        assert!(guard_query(r#"mysql_global_status_connections"#).is_err());
        assert!(guard_query(r#"sum(rate(nginx_http_requests_total[5m]))"#).is_err());
        assert!(guard_query(r#"sum by (job_id) (mysql_x)"#).is_err());
    }
}
