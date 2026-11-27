//! 写操作单飞（防连点/重试）与生命周期守卫。

#![allow(unused_imports)]
use std::sync::Arc;

use crate::state::AppState;

/// 写操作单飞 + 生命周期守卫。
///
/// 起因（线上实测）：3 个并发 `POST …/restart` 全部 200，`restartNonce` 连加 3，
/// operator 连做 3 轮滚动升级，最后一轮引用了已被 retention 清掉的 checkpoint，
/// 作业直接 `FAILED`。这里做两件事：
/// 1. 同一 `METHOD path` 在 15s 内只允许一个请求在跑，重复请求 409；
/// 2. `lifecycleState` 处于 `UPGRADING / ROLLING_BACK` 时，拒绝再来一次重启/改规格。
pub(crate) async fn action_guard(
    axum::extract::State(st): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;
    use axum::response::IntoResponse;

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let is_write = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    // 只对"针对某一个具体作业"的写操作生效；POST /api/jobs（建作业）不占用槽位，
    // 否则同时建两个不同作业也会互相冲突。
    let parsed = if is_write {
        job_action_parts(&path)
    } else {
        None
    };
    let Some((cluster, ns, name, action)) = parsed else {
        return next.run(req).await;
    };

    let key = format!("{method} {path}");
    let _guard = match st.begin_action(&key, std::time::Duration::from_secs(15)) {
        Ok(g) => g,
        Err(e) => {
            return (
                axum::http::StatusCode::CONFLICT,
                axum::Json(serde_json::json!({
                    "error": { "code": "conflict", "message": e.to_string() }
                })),
            )
                .into_response()
        }
    };

    // 升级/回滚进行中：再点一次重启只会打断调谐；DELETE 会留下半套工作负载，也一起拦。
    let guarded = matches!(
        action.as_deref(),
        Some("restart" | "state" | "image" | "scale" | "restore" | "rollback" | "cancel")
    ) || method == Method::DELETE;
    if guarded {
        if let Ok(c) = st.cluster(&cluster).await {
            if let Ok(obj) = c
                .get_raw(flinkops_types::crd::KIND_DEPLOYMENT, &ns, &name)
                .await
            {
                let lifecycle = flinkops_types::crd::s(&obj, &["status", "lifecycleState"])
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                if matches!(lifecycle.as_str(), "UPGRADING" | "ROLLING_BACK") {
                    return (
                        axum::http::StatusCode::CONFLICT,
                        axum::Json(serde_json::json!({
                            "error": { "code": "conflict", "message": format!(
                                "{ns}/{name} 当前 lifecycleState={lifecycle}（升级/回滚进行中），请等状态稳定后再操作"
                            ) }
                        })),
                    )
                        .into_response();
                }
            }
        }
    }
    next.run(req).await
}

/// `/api/jobs/{cluster}/{ns}/{name}[/{action}]` → (cluster, ns, name, action)
pub(crate) fn job_action_parts(path: &str) -> Option<(String, String, String, Option<String>)> {
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if p.len() < 5 || p[0] != "api" || p[1] != "jobs" {
        return None;
    }
    Some((
        p[2].to_string(),
        p[3].to_string(),
        p[4].to_string(),
        p.get(5).map(|s| s.to_string()),
    ))
}
