//! 失败请求审计：403/409/5xx 都留痕，含 error.message。

#![allow(unused_imports)]
use std::sync::Arc;

use crate::state::AppState;

/// 写操作的失败也要进审计：越权、冲突、上游报错原来一条都不留。
pub(crate) async fn audit_failures(
    axum::extract::State(st): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let is_write = matches!(
        method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    );
    let actor = if is_write {
        st.user(req.headers()).await
    } else {
        None
    };
    let resp = next.run(req).await;
    if is_write && resp.status().is_client_error() {
        let code = resp.status().as_u16();
        // 401/404 太吵（扫描器/敲错路径），只记真实的业务失败
        if code != 401 && code != 404 {
            // 把响应体里的人类可读原因也记下来：只写 status=400 等于没写
            let (parts, body) = resp.into_parts();
            let bytes = axum::body::to_bytes(body, 8 * 1024)
                .await
                .unwrap_or_default();
            let reason = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            st.audit(
                actor.as_deref().unwrap_or("anonymous"),
                "http.error",
                &path,
                "",
                "",
                "error",
                Some(if reason.is_empty() {
                    format!("status={code}")
                } else {
                    format!("status={code} {reason}")
                }),
                None,
            );
            return axum::response::Response::from_parts(parts, axum::body::Body::from(bytes));
        }
    }
    resp
}
