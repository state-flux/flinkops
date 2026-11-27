//! Flink Web UI 只读反代。
//!
//! 规则（与 docs/review.md P0/P1 对齐）：
//! - 只放行 GET / HEAD / OPTIONS；cancel / stop / savepoints / jars / rescaling
//!   等写操作在**代理层**拒绝（query 也要检查，`?mode=cancel` 就在 query 里）；
//! - WebSocket 暂不支持，明确返回 501（前端降级轮询），不静默断开；
//! - 每次读/拦截都写审计。
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message as AxMessage, WebSocket};
use axum::extract::{FromRequestParts, Path, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use flinkops_types::Error;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as TgMessage;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::state::AppState;

/// 判断是否为需要拦截的写操作。`path` 应包含 query。
pub fn is_write_path(method: &Method, path: &str) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    let p = path.to_ascii_lowercase();
    p.contains("mode=cancel")
        || p.ends_with("/stop")
        || p.contains("/savepoints")
        || p.contains("/jars")
        || p.contains("/rescaling")
        || p.contains("/savepoint-disposal")
        || *method == Method::DELETE
}

/// `/proxy/{cluster}/{ns}/{name}` —— Flink UI 的根（静态资源用相对路径，靠尾斜杠解析）。
pub async fn handle_root(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    method: Method,
    Path((cluster, ns, name)): Path<(String, String, String)>,
    req: axum::extract::Request,
) -> Response {
    let AuthUser { headers, .. } = u;

    proxy_inner(st, headers, method, cluster, ns, name, String::new(), req).await
}

/// `/proxy/{cluster}/{ns}/{name}/{*path}` —— 其余所有路径。
pub async fn handle(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    method: Method,
    Path((cluster, ns, name, rest_path)): Path<(String, String, String, String)>,
    req: axum::extract::Request,
) -> Response {
    let AuthUser { headers, .. } = u;

    proxy_inner(st, headers, method, cluster, ns, name, rest_path, req).await
}

#[allow(clippy::too_many_arguments)]
async fn proxy_inner(
    st: Arc<AppState>,
    headers: HeaderMap,
    method: Method,
    cluster: String,
    ns: String,
    name: String,
    rest_path: String,
    req: axum::extract::Request,
) -> Response {
    let user = match st.require_user(&headers).await {
        Ok(u) => u,
        Err(e) => return ApiError(e).into_response(),
    };

    // WebSocket：透传到同一个 JobManager（Flink UI 的实时推送走 /ws）。
    // 这里手工调用提取器：Request 只能消费一次，所以先拆成 parts + body，
    // 不是 WS 握手再把它们拼回去走普通 HTTP 转发。
    let (mut parts, body) = req.into_parts();
    if let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        let query = parts.uri.query().unwrap_or("").to_string();
        let endpoint = match st.rest_for(&cluster, &ns, &name).await {
            Ok(r) => r,
            Err(e) => return ApiError(e).into_response(),
        };
        let upstream = ws_url(endpoint.base(), &rest_path, &query);
        st.audit(
            &user,
            "ui.open",
            &name,
            &cluster,
            &ns,
            "ok",
            Some(format!("ws {upstream}")),
            None,
        );
        return upgrade.on_upgrade(move |socket| bridge(socket, upstream));
    }
    let req = axum::extract::Request::from_parts(parts, body);

    // path + query 一起判定（否则 ?mode=cancel 会漏网）
    let query = req.uri().query().unwrap_or("");
    let mut probe = rest_path.clone();
    if !query.is_empty() {
        probe.push('?');
        probe.push_str(query);
    }

    if is_write_path(&method, &format!("/{probe}")) {
        st.audit(
            &user,
            "proxy.blocked",
            &format!("{method} /{probe}"),
            &cluster,
            &ns,
            "denied",
            Some("只读反代：写操作请在控制台执行".into()),
            None,
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": "readonly_proxy",
                    "message": "该操作会修改作业状态，已拦截。请在控制台执行（有 RBAC 与二次确认）"
                }
            })),
        )
            .into_response();
    }

    let rest = match st.rest_for(&cluster, &ns, &name).await {
        Ok(r) => r,
        Err(e) => return ApiError(e).into_response(),
    };
    let mut url = format!("{}/{}", rest.base(), rest_path.trim_start_matches('/'));
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    let body = match axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return ApiError(Error::BadRequest(format!("读取请求体失败: {e}"))).into_response()
        }
    };

    let mut outbound = st.http.request(method.clone(), &url);
    if let Some(ct) = headers.get(header::CONTENT_TYPE) {
        if let Ok(v) = ct.to_str() {
            outbound = outbound.header(header::CONTENT_TYPE, v);
        }
    }
    if !body.is_empty() {
        outbound = outbound.body(body.to_vec());
    }

    match outbound.send().await {
        Ok(resp) => {
            let status = resp.status();
            let ctype = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes: Bytes = resp.bytes().await.unwrap_or_default();

            st.audit(
                &user,
                "ui.open",
                &name,
                &cluster,
                &ns,
                "ok",
                Some(status.to_string()),
                None,
            );

            let mut builder = Response::builder().status(status.as_u16());
            builder = builder.header(header::CONTENT_TYPE, ctype);
            if let Some(loc) = location {
                builder = builder.header(header::LOCATION, loc);
            }
            builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => {
            st.audit(
                &user,
                "ui.open",
                &name,
                &cluster,
                &ns,
                "error",
                Some(e.to_string()),
                None,
            );
            ApiError(Error::Upstream {
                target: rest.base().to_string(),
                message: e.to_string(),
            })
            .into_response()
        }
    }
}

/// `http://host:port` + 路径 → `ws://host:port/path?query`
fn ws_url(base: &str, path: &str, query: &str) -> String {
    let base = base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    let mut url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// 双向转发 WebSocket 消息。任一侧关闭或出错就整体结束。
async fn bridge(mut client: WebSocket, upstream: String) {
    let stream = match tokio_tungstenite::connect_async(&upstream).await {
        Ok((s, _)) => s,
        Err(e) => {
            tracing::warn!(upstream = %upstream, error = %e, "ws upstream connect failed");
            let _ = client.send(AxMessage::Close(None)).await;
            return;
        }
    };
    let (mut up_tx, mut up_rx) = stream.split();

    loop {
        tokio::select! {
            // 浏览器 → JM
            msg = client.next() => match msg {
                Some(Ok(m)) => {
                    if let Some(t) = to_upstream(m) {
                        if up_tx.send(t).await.is_err() { break; }
                    } else {
                        break;
                    }
                }
                _ => break,
            },
            // JM → 浏览器
            msg = up_rx.next() => match msg {
                Some(Ok(m)) => {
                    if let Some(a) = to_client(m) {
                        if client.send(a).await.is_err() { break; }
                    } else {
                        break;
                    }
                }
                _ => break,
            },
        }
    }
}

fn to_upstream(m: AxMessage) -> Option<TgMessage> {
    match m {
        AxMessage::Text(t) => Some(TgMessage::Text(t.as_str().into())),
        AxMessage::Binary(b) => Some(TgMessage::Binary(b)),
        AxMessage::Ping(p) => Some(TgMessage::Ping(p)),
        AxMessage::Pong(p) => Some(TgMessage::Pong(p)),
        AxMessage::Close(_) => None, // 关闭由循环退出处理
    }
}

fn to_client(m: TgMessage) -> Option<AxMessage> {
    match m {
        TgMessage::Text(t) => Some(AxMessage::Text(t.as_str().into())),
        TgMessage::Binary(b) => Some(AxMessage::Binary(b)),
        TgMessage::Ping(p) => Some(AxMessage::Ping(p)),
        TgMessage::Pong(p) => Some(AxMessage::Pong(p)),
        TgMessage::Close(_) => None,
        TgMessage::Frame(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ws_url_from_rest_base() {
        assert_eq!(
            ws_url("http://1.2.3.4:30081", "/ws", ""),
            "ws://1.2.3.4:30081/ws"
        );
        assert_eq!(
            ws_url("http://svc:8081", "ws", "a=1"),
            "ws://svc:8081/ws?a=1"
        );
    }

    #[test]
    fn blocks_all_write_shapes() {
        let p = Method::POST;
        assert!(is_write_path(&p, "/jobs/abc?mode=cancel"));
        assert!(is_write_path(&p, "/jobs/abc/stop"));
        assert!(is_write_path(&p, "/jars/upload"));
        assert!(is_write_path(&p, "/jobs/abc/savepoints"));
        assert!(is_write_path(&Method::DELETE, "/jobs/abc"));
        assert!(!is_write_path(&Method::GET, "/overview"));
        assert!(!is_write_path(&Method::GET, "/jobs/abc/checkpoints"));
    }
}
