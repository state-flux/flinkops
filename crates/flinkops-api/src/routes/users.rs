//! 多租户账号管理（仅 admin）。
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertBody {
    pub username: String,
    /// 新建必填；修改时留空表示不改密码
    #[serde(default)]
    pub password: Option<String>,
    /// admin | operator | viewer
    pub role: String,
    #[serde(default)]
    pub allow_ns: Vec<String>,
}

pub async fn list(State(st): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    st.require_admin(&headers).await.map_err(ApiError)?;
    let users = st.store.list_users().await?;
    let items: Vec<Value> = users
        .into_iter()
        .map(|u| {
            json!({
                "username": u.username,
                "role": u.role,
                "allowNs": u.allow_ns,
                "disabled": u.disabled,
                "createdAt": u.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "count": items.len() })))
}

pub async fn save(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UpsertBody>,
) -> ApiResult<Json<Value>> {
    let actor = st.require_admin(&headers).await.map_err(ApiError)?;
    let username = body.username.trim().to_string();
    if username.is_empty() {
        return Err(ApiError(Error::BadRequest("username 不能为空".into())));
    }
    if !["admin", "operator", "viewer"].contains(&body.role.as_str()) {
        return Err(ApiError(Error::BadRequest(
            "role 只能是 admin / operator / viewer".into(),
        )));
    }
    let existing = st.store.get_user(&username).await?;
    if existing.is_none() && body.password.as_deref().unwrap_or("").is_empty() {
        return Err(ApiError(Error::BadRequest("新建账号必须设置密码".into())));
    }
    // 不允许把最后一个 admin 降级
    if let Some(prev) = &existing {
        if prev.role == "admin" && body.role != "admin" && st.store.count_admins().await? <= 1 {
            return Err(ApiError(Error::Conflict("至少要保留一个 admin".into())));
        }
    }
    let hash = body
        .password
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(flinkops_store::hash_password);
    st.store
        .upsert_user(&username, hash.as_deref(), &body.role, &body.allow_ns)
        .await?;
    st.audit(
        &actor.user,
        "user.save",
        &username,
        "",
        "",
        "ok",
        Some(format!("role={} ns={:?}", body.role, body.allow_ns)),
        None,
    );
    Ok(Json(json!({ "ok": true, "username": username })))
}

pub async fn remove(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> ApiResult<Json<Value>> {
    let actor = st.require_admin(&headers).await.map_err(ApiError)?;
    if username == actor.user {
        return Err(ApiError(Error::BadRequest("不能删除当前登录账号".into())));
    }
    if let Some(u) = st.store.get_user(&username).await? {
        if u.role == "admin" && st.store.count_admins().await? <= 1 {
            return Err(ApiError(Error::Conflict("至少要保留一个 admin".into())));
        }
    }
    let n = st.store.delete_user(&username).await?;
    if n == 0 {
        return Err(ApiError(Error::NotFound(format!("用户 {username}"))));
    }
    st.audit(
        &actor.user,
        "user.delete",
        &username,
        "",
        "",
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "deleted": username })))
}
