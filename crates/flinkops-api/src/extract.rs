//! 请求级提取器：把"鉴权 + 只读检查 + 作业三元组解析"从每个 handler 里抽出来。
//!
//! 背景：22 个作业端点曾各自手抄
//! `let user = st.require_user(&headers).await?;` + `if st.cfg.readonly {...}`，
//! 共 105 处 `require_user`、34 处只读检查 —— 漏抄一处就是越权或"只读平台仍能写"。
//! 提取器让"忘记校验"从**语义上不可能**：handler 想要用户就必须声明 `AuthUser`。
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::http::HeaderMap;

use crate::error::ApiError;
use crate::state::{AppState, Principal};
use flinkops_types::Error;

/// 已登录用户（附带原始 HeaderMap：MCP 闸门与审计还要用）。
pub struct AuthUser {
    pub user: String,
    pub principal: Principal,
    /// 原始请求头（gate()/代理/审计需要）
    pub headers: HeaderMap,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        st: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let headers = parts.headers.clone();
        let user = st.require_user(&headers).await.map_err(ApiError)?;
        // principal 理论上一定存在（require_user 刚验过会话），兜底成最小权限
        let principal = st.principal(&headers).await.unwrap_or(Principal {
            user: user.clone(),
            role: "viewer".into(),
            allow_ns: Vec::new(),
        });
        Ok(Self {
            user,
            principal,
            headers,
        })
    }
}

/// 平台可写（非只读模式）。声明它等于声明"这个 handler 会写"。
pub struct RequireWrite;

impl FromRequestParts<Arc<AppState>> for RequireWrite {
    type Rejection = ApiError;

    async fn from_request_parts(
        _parts: &mut Parts,
        st: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if st.cfg.readonly {
            return Err(ApiError(Error::Forbidden("平台处于只读模式".into())));
        }
        Ok(RequireWrite)
    }
}

/// `/api/jobs/{cluster}/{ns}/{name}/...` 的三元组。
pub struct JobTarget {
    pub cluster: String,
    pub ns: String,
    pub name: String,
}

impl FromRequestParts<Arc<AppState>> for JobTarget {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        st: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path((cluster, ns, name)) =
            Path::<(String, String, String)>::from_request_parts(parts, st)
                .await
                .map_err(|_| {
                    ApiError(Error::BadRequest(
                        "路径需要 /{cluster}/{namespace}/{name}".into(),
                    ))
                })?;
        Ok(Self { cluster, ns, name })
    }
}
