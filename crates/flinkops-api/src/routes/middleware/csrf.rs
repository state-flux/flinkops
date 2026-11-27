//! CSRF 双提交校验（写操作必须带 X-CSRF-Token）。

#![allow(unused_imports)]
use std::sync::Arc;

use crate::state::AppState;

/// CSRF 双提交校验：登录时下发非 HttpOnly 的 `flinkops_csrf`，
/// 前端把同一个值放进 `X-CSRF-Token`；两者不一致就拒绝。
///
/// 为什么不用 `SameSite=Strict` 就够了：Lax 仍允许顶级导航式 POST，
/// 而且平台被嵌在门户/iframe 里时 SameSite 语义不可靠。双提交是零存储的兜底。
pub(crate) async fn csrf_guard(
    axum::extract::State(_st): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;
    use axum::response::IntoResponse;

    let method = req.method().clone();
    if !matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    if !path.starts_with("/api/")
        || path.starts_with("/api/login")
        || crate::mcp_guard::is_mcp(req.headers())
    {
        return next.run(req).await;
    }
    // 没有会话 Cookie → 交给 rbac_guard 出 401（这里不抢答）
    let headers = req.headers();
    if crate::state::cookie(headers, crate::state::SESSION_COOKIE).is_none() {
        return next.run(req).await;
    }
    let cookie_token = crate::state::cookie(headers, crate::state::CSRF_COOKIE).unwrap_or_default();
    let header_token = headers
        .get(crate::state::CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    let ok = !cookie_token.is_empty()
        && cookie_token.len() == header_token.len()
        && cookie_token
            .bytes()
            .zip(header_token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !ok {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": {
                    "code": "csrf",
                    "message": "缺少或错误的 CSRF 令牌：请刷新页面（会话建立时会下发），或重新登录"
                }
            })),
        )
            .into_response();
    }
    next.run(req).await
}
