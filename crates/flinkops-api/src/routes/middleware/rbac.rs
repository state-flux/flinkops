//! 多租户闸门：角色 + 命名空间白名单。

#![allow(unused_imports)]
use std::sync::Arc;

use crate::state::AppState;

/// 多租户闸门：角色 + 命名空间白名单。
///
/// - 账号密码建会话后，每次请求都按库里的角色判定（改角色即时生效）；
/// - viewer 只能读；operator 只能写自己白名单里的 namespace；
/// - admin 区域（账号/Token/审批/设置/诊断/集群接入）只对 admin 开放；
/// - MCP Bearer 走自己的 `mcp_guard`，这里跳过避免双重判定。
pub(crate) async fn rbac_guard(
    axum::extract::State(st): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;
    use axum::response::IntoResponse;

    let path = req.uri().path().to_string();
    // 非 API 路径（SPA 的 index.html、/assets/*.js、前端路由 /login /jobs…）一律放行：
    // 登录页本身必须打得开，否则首次访问/会话过期就只能看到一页 JSON 401。
    // 真正的鉴权在 API 层（本中间件后续分支）与前端路由守卫里。
    // `/proxy/*` 例外：它代表"某个作业的 Flink UI"，同样要过身份与 ns 白名单。
    if !path.starts_with("/api/") && !path.starts_with("/proxy/") {
        return next.run(req).await;
    }
    let skip = path.starts_with("/api/login")
        || path.starts_with("/api/logout")
        || path == "/api/me"
        || path.starts_with("/api/version")
        || path.starts_with("/healthz")
        || path.starts_with("/readyz")
        || crate::mcp_guard::is_mcp(req.headers());
    if skip {
        return next.run(req).await;
    }

    let Some(principal) = st.principal(req.headers()).await else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": { "code": "unauthorized", "message": "会话已失效，请重新登录" }
            })),
        )
            .into_response();
    };

    let is_write = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let admin_area = path.starts_with("/api/users")
        || path.starts_with("/api/tokens")
        || path.starts_with("/api/approvals")
        || path.starts_with("/api/settings")
        || path.starts_with("/api/admin")
        || path.starts_with("/api/clusters")
        // 审计里有全集群的 target/jarURI/namespace，不能让普通账号读
        || path.starts_with("/api/audit");
    let deny = |msg: String| {
        (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": { "code": "forbidden", "message": msg }
            })),
        )
            .into_response()
    };

    if admin_area && !principal.is_admin() {
        return deny(format!(
            "{path} 需要 admin 角色（当前 {} 是 {}）",
            principal.user, principal.role
        ));
    }
    // 自助服务（改自己的密码 / 开关自己的 2FA）不要求写权限：
    // viewer 也应该能改密码，否则只能找管理员重置。
    let self_service = path.starts_with("/api/me/");
    if is_write && !principal.can_write() && !self_service {
        return deny(format!("{} 是只读角色，不能执行写操作", principal.user));
    }
    // 读也要过 ns 白名单：否则"限定 team-a 的账号"能直接 GET 到 team-b 的
    // 详情 / YAML / 日志 / 事件 / 快照（实测全 200）。
    if let Some(ns) = ns_from_api_path(&path) {
        if !principal.ns_ok(&ns) {
            return deny(format!(
                "{} 无权访问命名空间 {ns}（白名单：{}）",
                principal.user,
                if principal.allow_ns.is_empty() {
                    "空".to_string()
                } else {
                    principal.allow_ns.join(",")
                }
            ));
        }
    }
    next.run(req).await
}

/// 从 `/api/{资源}/{cluster}/{ns}/...` 或 `/proxy/{cluster}/{ns}/...` 里取 namespace。
pub(crate) fn ns_from_api_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // ["proxy", cluster, ns, name, ...]
    if parts.len() >= 4 && parts[0] == "proxy" {
        return Some(parts[2].to_string());
    }
    // ["api", "jobs", cluster, ns, name, ...]
    if parts.len() >= 5
        && parts[0] == "api"
        && matches!(parts[1], "jobs" | "snapshots" | "session-jobs" | "sessions")
    {
        return Some(parts[3].to_string());
    }
    None
}
