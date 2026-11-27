//! MCP API Token 管理（浏览器会话操作，见 `docs/mcp-spec.md §9`）。
//!
//! secret 只在创建时返回一次；库里只存 `sha256(secret)`。
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_types::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};
use crate::mcp_guard::{sha256_hex, TOKEN_PREFIX};
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateToken {
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub allow_namespaces: Vec<String>,
    #[serde(default)]
    pub allow_jobs: Vec<String>,
    pub qps: Option<i64>,
    pub expires_in_days: Option<i64>,
}

pub async fn create(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Json(req): Json<CreateToken>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    if req.name.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("name 不能为空".into())));
    }
    let scopes: Vec<String> = if req.scopes.is_empty() {
        vec!["read".into()]
    } else {
        req.scopes.clone()
    };
    for s in &scopes {
        if !["read", "operate", "admin"].contains(&s.as_str()) {
            return Err(ApiError(Error::BadRequest(format!("未知 scope: {s}"))));
        }
    }
    // 空范围 = 无权：创建时就要求至少给一个命名空间或作业
    if req.allow_namespaces.is_empty() && req.allow_jobs.is_empty() {
        return Err(ApiError(Error::BadRequest(
            "必须至少指定 allowNamespaces 或 allowJobs（空 = 无权，见 mcp-spec §2.3）".into(),
        )));
    }
    if req
        .allow_jobs
        .iter()
        .any(|pattern| !pattern.contains('/') && pattern != "*")
    {
        return Err(ApiError(Error::BadRequest(
            "allowJobs 要用 <namespace>/<name> 形式（或 *）".into(),
        )));
    }

    let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let expires_at = req.expires_in_days.map(|d| {
        (chrono::Utc::now() + chrono::Duration::days(d.max(1)))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
    st.store
        .insert_mcp_token(
            &id,
            req.name.trim(),
            &sha256_hex(&secret),
            &scopes,
            &req.allow_namespaces,
            &req.allow_jobs,
            req.qps.unwrap_or(5).clamp(1, 100),
            expires_at.as_deref(),
        )
        .await?;
    st.audit(
        &user,
        "token.create",
        &id,
        "",
        "",
        "ok",
        Some(format!("name={} scopes={:?}", req.name.trim(), scopes)),
        None,
    );
    Ok(Json(json!({
        "id": id,
        "token": format!("{TOKEN_PREFIX}{id}_{secret}"),
        "note": "secret 只显示这一次，请立即保存",
    })))
}

pub async fn list(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    let items: Vec<Value> = st
        .store
        .list_mcp_tokens()
        .await?
        .into_iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "scopes": t.scopes,
                "allowNamespaces": t.allow_ns,
                "allowJobs": t.allow_jobs,
                "qps": t.qps,
                "expiresAt": t.expires_at,
                "createdAt": t.created_at,
                "lastUsedAt": t.last_used_at,
                "revokedAt": t.revoked_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

pub async fn revoke(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;

    let ok = st.store.revoke_mcp_token(&id).await?;
    st.audit(
        &user,
        "token.revoke",
        &id,
        "",
        "",
        if ok { "ok" } else { "denied" },
        None,
        None,
    );
    if !ok {
        return Err(ApiError(Error::NotFound(format!(
            "token {id} 不存在或已吊销"
        ))));
    }
    Ok(Json(json!({ "ok": true })))
}

pub fn _assert_result(_: Result<()>) {}
