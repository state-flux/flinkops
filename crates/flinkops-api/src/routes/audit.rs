//! 审计日志：分页 + 过滤 + CSV 导出。
//!
//! 原来只有"最近 200 条"：合规审计要按人/动作/时间翻，事故复盘要导出给对方。
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const DEFAULT_LIMIT: i64 = 200;

fn csv_escape(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

pub async fn list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let _ = st.require_admin(&headers).await.map_err(ApiError)?;

    let action = q.get("action").map(String::as_str);
    let actor = q.get("actor").map(String::as_str);
    let result = q.get("result").map(String::as_str);
    let namespace = q.get("namespace").map(String::as_str);
    let keyword = q.get("q").map(String::as_str);
    let since = q.get("since").map(String::as_str);
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_LIMIT);
    let offset = q
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let filter = flinkops_store::AuditFilter {
        action: action.map(str::to_string),
        actor: actor.map(str::to_string),
        result: result.map(str::to_string),
        namespace: namespace.map(str::to_string),
        keyword: keyword.map(str::to_string),
        since: since.map(str::to_string),
        limit,
        offset,
    };
    let (items, total) = st.store.list_audit_filtered(&filter).await?;

    if q.get("format").map(|f| f == "csv").unwrap_or(false) {
        let mut out = String::from("\u{feff}");
        out.push_str("ts,actor,action,target,cluster,namespace,result,ip,detail\n");
        for e in &items {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&e.ts),
                csv_escape(&e.actor),
                csv_escape(&e.action),
                csv_escape(&e.target),
                csv_escape(&e.cluster),
                csv_escape(&e.namespace),
                csv_escape(&e.result),
                csv_escape(e.ip.as_deref().unwrap_or("")),
                csv_escape(e.detail.as_deref().unwrap_or("")),
            ));
        }
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"flinkops-audit.csv\"",
                ),
            ],
            out,
        )
            .into_response());
    }

    Ok(Json(json!({
        "items": items,
        "count": items.len(),
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
    .into_response())
}
