//! 路由装配。**新增接口时只改这里 + 对应模块**（见 docs/api-spec.md §新增接口五步）。
mod middleware;

use middleware::{
    action_guard, api_not_found, audit_failures, csrf_guard, normalize_rejection, rbac_guard,
};

use std::sync::Arc;

use axum::http::{header, HeaderValue};
use axum::routing::{any, delete, get, patch, post, put};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub mod admin;
pub mod alerts;
pub mod approvals;
pub mod audit;
pub mod auth;
pub mod catalogs;
pub mod cluster_info;
pub mod clusters;
pub mod deploy_templates;
pub mod editor;
pub mod export;
pub mod jobs;
pub mod metrics;
pub mod observability;
pub mod profiles;
pub mod proxy;
pub mod session_jobs;
pub mod sessions;
pub mod settings;
pub mod snapshots;
pub mod sql;
pub mod system;
pub mod templates;
pub mod tokens;
pub mod users;

pub fn build(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // ---- 系统
        .route("/healthz", get(system::healthz))
        .route("/readyz", get(system::readyz))
        .route("/api/version", get(system::version))
        .route("/api/cluster-info", get(cluster_info::info))
        // ---- 认证
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/me/password", post(auth::change_password))
        .route("/api/me/2fa/setup", post(auth::totp_setup))
        .route("/api/me/2fa/enable", post(auth::totp_enable))
        .route("/api/me/2fa/disable", post(auth::totp_disable))
        // ---- 多租户账号（仅 admin）
        .route("/api/users", get(users::list).post(users::save))
        .route("/api/users/{username}", delete(users::remove))
        // ---- MCP：机器 Token 与一次性审批（见 docs/mcp-spec.md §9）
        .route("/api/tokens", get(tokens::list).post(tokens::create))
        .route("/api/tokens/{id}", delete(tokens::revoke))
        .route("/api/approvals", get(approvals::list))
        .route("/api/approvals/stream", get(approvals::stream))
        .route("/api/approvals/{id}", patch(approvals::decide))
        // ---- 作业
        .route("/api/jobs", get(jobs::list).post(jobs::create))
        .route("/api/jobs/sync", post(jobs::sync))
        .route(
            "/api/jobs/{cluster}/{ns}/{name}",
            get(jobs::detail)
                .put(jobs::update_job)
                .delete(jobs::delete_job),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/metrics",
            get(jobs::metrics),
        )
        .route("/api/jobs/{cluster}/{ns}/{name}/log", get(jobs::log))
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/pod-log",
            get(jobs::pod_log),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/state",
            post(jobs::set_state),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/restart",
            post(jobs::restart),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/savepoint",
            post(jobs::savepoint),
        )
        .route("/api/jobs/{cluster}/{ns}/{name}/events", get(jobs::events))
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/exceptions",
            get(jobs::exceptions),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/backpressure",
            get(jobs::backpressure),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/flamegraph",
            get(jobs::flamegraph),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/profile",
            post(jobs::profile),
        )
        // ---- 平台
        .route("/api/profiles", get(profiles::list))
        .route("/api/profiles/{id}", put(profiles::save))
        .route("/api/audit", get(audit::list))
        // ---- 告警
        .route(
            "/api/alerts/rules",
            get(alerts::list_rules).post(alerts::save_rule),
        )
        .route(
            "/api/alerts/rules/{id}",
            axum::routing::delete(alerts::delete_rule),
        )
        .route("/api/alerts/events", get(alerts::list_events))
        .route("/api/alerts/events/{id}/ack", post(alerts::ack_event))
        .route("/api/sessions", get(sessions::list).post(sessions::create))
        .route(
            "/api/sessions/{cluster}/{ns}/{name}/sql-gateway",
            post(sessions::deploy_sql_gateway),
        )
        // ---- FlinkSessionJob（作业提交到 session cluster 的官方方式）
        .route(
            "/api/session-jobs",
            get(session_jobs::list).post(session_jobs::create),
        )
        .route(
            "/api/session-jobs/{cluster}/{ns}/{name}",
            axum::routing::delete(session_jobs::remove),
        )
        // ---- 作业模板
        .route(
            "/api/templates",
            get(templates::list).post(templates::create),
        )
        .route(
            "/api/templates/{id}",
            axum::routing::delete(templates::remove).put(templates::update),
        )
        .route("/api/snapshots", get(snapshots::list))
        .route(
            "/api/snapshots/{cluster}/{ns}/{name}",
            axum::routing::delete(snapshots::remove),
        )
        // ---- 运维
        .route("/api/admin/vacuum", post(admin::vacuum))
        .route("/api/admin/backup", get(admin::backup))
        .route("/api/admin/diagnostic", get(admin::diagnostic))
        .route("/api/admin/crd-schema/{name}", get(admin::crd_schema))
        // ---- 运行期配置
        .route("/api/settings", get(settings::get).post(settings::save))
        // ---- SQL 工作台
        .route("/api/sql/execute", post(sql::execute))
        .route(
            "/api/sql/gateway/{cluster}/{ns}/{session}",
            get(sql::gateway_info),
        )
        // ---- SQL 作业（VVR 式：存 SQL + 版本 + 提交/删除）
        .route("/api/sql/jobs", get(sql::list_jobs).post(sql::create_job))
        .route(
            "/api/sql/jobs/{id}",
            axum::routing::put(sql::edit_job).delete(sql::delete_job),
        )
        .route("/api/sql/jobs/{id}/deploy", post(sql::deploy_job))
        .route("/api/sql/jobs/{id}/deploy-as-app", post(sql::deploy_as_app))
        .route(
            "/api/sql/jobs/{id}/deploy-as-sessionjob",
            post(sql::deploy_as_sessionjob),
        )
        .route("/api/sql/jobs/{id}/suspend", post(sql::suspend_job))
        .route("/api/sql/jobs/{id}/resume", post(sql::resume_job))
        .route("/api/sql/jobs/{id}/versions", get(sql::versions))
        // ---- 导出（xlsx 走 python openpyxl）
        .route("/api/export/xlsx", post(export::xlsx))
        .route(
            "/api/sql/catalog/{cluster}/{ns}/{session}",
            get(sql::catalog),
        )
        // ---- 外部数据源元数据（StarRocks/MySQL/Doris）
        .route(
            "/api/catalogs",
            get(catalogs::list).post(catalogs::register),
        )
        // ---- 部署模板（产出 FlinkDeployment 的 YAML，可自定义）
        .route(
            "/api/deploy-templates",
            get(deploy_templates::list).post(deploy_templates::save),
        )
        .route(
            "/api/deploy-templates/{id}",
            axum::routing::delete(deploy_templates::remove),
        )
        .route(
            "/api/catalogs/{id}",
            axum::routing::delete(catalogs::remove),
        )
        .route("/api/catalogs/{id}/sync", post(catalogs::sync))
        .route("/api/catalogs/{id}/tables", get(catalogs::tables))
        .route("/api/catalogs/{id}/diff", get(catalogs::diff))
        .route("/api/catalogs/complete", get(catalogs::complete))
        .route("/api/settings/reset", post(settings::reset))
        .route("/api/clusters/{id}/credential", get(admin::credential))
        // ---- 多集群动态接入
        .route("/api/clusters", post(clusters::add))
        .route("/api/clusters/test", post(clusters::test))
        .route(
            "/api/clusters/{id}",
            axum::routing::delete(clusters::remove).patch(clusters::set_readonly),
        )
        .route("/api/clusters/{id}/reconnect", post(clusters::reconnect))
        // ---- Prometheus（历史与聚合；平台不存指标点）
        .route("/api/metrics/query", get(metrics::query))
        .route("/api/metrics/query_range", get(metrics::query_range))
        .route("/api/prometheus/discover", get(metrics::discover))
        // ---- 监控接入下发
        .route(
            "/api/observability/podmonitor",
            post(observability::apply_podmonitor),
        )
        .route(
            "/api/observability/patch-labels",
            post(observability::patch_labels),
        )
        // ---- 编辑器支撑（YAML / 校验 / diff / 模板渲染）
        .route("/api/validate", post(editor::validate))
        .route("/api/format", post(editor::format))
        .route("/api/diff", post(editor::diff))
        .route("/api/render", post(editor::render))
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/yaml",
            get(editor::job_yaml),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/image",
            post(jobs::set_image),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/restore",
            post(jobs::restore),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/cancel",
            post(jobs::cancel_job),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/rollback",
            post(jobs::rollback),
        )
        .route("/api/jobs/{cluster}/{ns}/{name}/scale", post(jobs::scale))
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/ingress",
            post(jobs::create_ingress),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/autoscaler",
            post(jobs::set_autoscaler),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/rest-exposed",
            post(jobs::set_rest_exposed),
        )
        .route(
            "/api/jobs/{cluster}/{ns}/{name}/blue-green",
            post(jobs::create_blue_green).get(jobs::blue_green_status),
        )
        // ---- 只读反代（Flink Web UI）
        .route("/proxy/{cluster}/{ns}/{name}", any(proxy::handle_root))
        .route("/proxy/{cluster}/{ns}/{name}/", any(proxy::handle_root))
        .route("/proxy/{cluster}/{ns}/{name}/{*path}", any(proxy::handle))
        // 未匹配的 /api/* 不能被 SPA fallback 接住（否则拼错接口会返回 200 HTML，
        // 前端 res.json() 直接抛 "Unexpected token <"，状态码也失去意义）
        .route("/api/{*rest}", any(api_not_found));

    let web_dir = state.cfg.web_dir.clone();
    Router::new()
        .merge(api)
        .fallback_service(
            // SPA：找不到静态文件就回落 index.html，由前端路由渲染
            ServeDir::new(&web_dir).fallback(ServeFile::new(
                std::path::Path::new(&web_dir).join("index.html"),
            )),
        )
        // 把 axum 内置提取器的拒绝（415/422 + text/plain）归一成项目统一错误信封。
        // 否则前端按 error.code 解析拿到 undefined，页面显示空白错误。
        .layer(axum::middleware::from_fn(normalize_rejection))
        // CSRF：写操作必须回带 X-CSRF-Token（双提交 Cookie）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            csrf_guard,
        ))
        // 多租户闸门（在审计层内层：被它拒绝的 403 也会进审计）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rbac_guard,
        ))
        // 写操作单飞 + 生命周期守卫（在 rbac 内层：先鉴权再占用动作槽）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            action_guard,
        ))
        // 失败也要留痕：原来只有成功路径写审计，越权/上游报错全无记录。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit_failures,
        ))
        // SPA 的 index.html 必须每次回源，否则改了前端、用户刷新还是旧 bundle；
        // assets 文件名带 hash，可以照常缓存。
        .layer(axum::middleware::map_response(
            |mut resp: axum::response::Response| async move {
                let is_html = resp
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.starts_with("text/html"))
                    .unwrap_or(false);
                if is_html {
                    resp.headers_mut().insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-cache, must-revalidate"),
                    );
                }
                resp
            },
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
