//! 错误信封统一化（含 404 兜底）。

#![allow(unused_imports)]
use std::sync::Arc;

use crate::state::AppState;
use axum::http::header;

/// 未匹配的 `/api/*`：返回统一的 404 信封，而不是 SPA 的 index.html。
pub(crate) async fn api_not_found() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": { "code": "not_found", "message": "接口不存在" }
        })),
    )
        .into_response()
}

/// 提取器拒绝归一化：415（缺 Content-Type）/ 422（字段缺失）→ 400 + JSON 信封。
pub(crate) async fn normalize_rejection(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let resp = next.run(req).await;
    let status = resp.status();
    // 415/422 是提取器拒绝；400 里也有一部分来自 `Json` 反序列化（body 不是合法 JSON），
    // 它们都是 text/plain，前端 res.json() 会直接抛错。三种一起归一。
    if status != axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE
        && status != axum::http::StatusCode::UNPROCESSABLE_ENTITY
        && status != axum::http::StatusCode::BAD_REQUEST
    {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let message = if parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false)
    {
        // 已经是 JSON 的错误（极少），原样返回
        return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
    } else if raw.trim().is_empty() {
        "请求不合法（缺少 Content-Type 或字段）".to_string()
    } else {
        format!("请求不合法：{}", raw.trim())
    };
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({
            "error": { "code": "bad_request", "message": message }
        })),
    )
        .into_response()
}
