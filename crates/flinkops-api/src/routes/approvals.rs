//! MCP 写操作审批（见 `docs/mcp-spec.md §4`）。
//!
//! 浏览器会话里批准/拒绝；批准时生成一次性 token（只回显一次），
//! MCP 侧把它放进 `X-FlinkOps-Approval` 头重试同一个调用。
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use flinkops_types::{Error, Result};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};
use crate::mcp_guard::sha256_hex;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// approve | reject
    pub decision: String,
}

pub async fn list(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items: Vec<Value> = st
        .store
        .list_mcp_approvals(q.status.as_deref())
        .await?
        .into_iter()
        .map(|a| {
            json!({
                "id": a.id,
                "tool": a.tool,
                "target": a.target,
                "argsHash": a.args_hash,
                "actor": a.actor,
                "tokenId": a.token_id,
                "status": a.status,
                "createdAt": a.created_at,
                "decidedAt": a.decided_at,
                "decidedBy": a.decided_by,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

pub async fn decide(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path(id): Path<String>,
    Json(req): Json<Decision>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    if st.store.get_mcp_approval(&id).await?.is_none() {
        return Err(ApiError(Error::NotFound(format!("审批 {id} 不存在"))));
    }
    // 超过窗口就不再受理：避免"放着不管，第二天点批准"
    if let Some(a) = st.store.get_mcp_approval(&id).await? {
        if a.status == "pending" && crate::mcp_guard::approval_expired(&a.created_at) {
            return Err(ApiError(Error::Conflict(format!(
                "该审批已超过 {}s 窗口，请让发起方重新调用",
                crate::mcp_guard::APPROVAL_TTL_SECS
            ))));
        }
    }
    let approve = match req.decision.as_str() {
        "approve" => true,
        "reject" => false,
        other => {
            return Err(ApiError(Error::BadRequest(format!(
                "decision 只能是 approve / reject，收到 {other}"
            ))))
        }
    };
    let mut token: Option<String> = None;
    let ok = if approve {
        let raw = format!("apv_{}", uuid::Uuid::new_v4().simple());
        let done = st
            .store
            .approve_mcp_approval(&id, &user, &sha256_hex(&raw))
            .await?;
        if done {
            token = Some(raw);
        }
        done
    } else {
        st.store.reject_mcp_approval(&id, &user).await?
    };
    st.audit(
        &user,
        "approval.decide",
        &id,
        "",
        "",
        if ok { "ok" } else { "denied" },
        Some(req.decision.clone()),
        None,
    );
    if !ok {
        return Err(ApiError(Error::Conflict(
            "该审批不是 pending 状态（可能已被处理/过期）".into(),
        )));
    }
    let _ = st.approvals_tx.send(
        json!({
            "type": "decided",
            "id": id,
            "decision": req.decision,
            "by": user,
        })
        .to_string(),
    );
    Ok(Json(json!({
        "ok": true,
        "approvalToken": token,
        "note": if approve { "把 approvalToken 放进 X-FlinkOps-Approval 头重试，一次有效" } else { "已拒绝" },
    })))
}

/// 审批事件流（SSE）：`GET /api/approvals/stream`。
///
/// 用 Cookie 鉴权（EventSource 天然带同源 Cookie）；浏览器断线会自动重连。
/// 事件：`hello`（建连确认）、`approval`（payload = `{type: pending|decided, …}`）。
pub async fn stream(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
) -> ApiResult<Sse<impl futures::Stream<Item = std::result::Result<Event, std::convert::Infallible>>>>
{
    let AuthUser { .. } = u;
    let rx = st.approvals_tx.subscribe();

    let tail = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(msg) => return Some((Ok(Event::default().event("approval").data(msg)), rx)),
                // 被丢弃的旧消息直接跳过，不要断开连接
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let head =
        futures::stream::once(async { Ok(Event::default().event("hello").data(r#"{"ok":true}"#)) });
    Ok(Sse::new(head.chain(tail)).keep_alive(KeepAlive::default()))
}

pub fn _unused(_: Result<()>) {}
