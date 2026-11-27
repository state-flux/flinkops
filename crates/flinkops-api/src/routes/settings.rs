//! 运行期可调配置。
//!
//! 只放**能热生效**的项（后台循环与 Prometheus 解析每次都读库），
//! 不放 bind / web_dir / admin 密码这类必须重启的项——避免"存了不生效"的假功能。
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

/// 可热改的键 + 默认值说明
const KEYS: [(&str, &str); 5] = [
    ("audit_retention_days", "审计保留天数（每小时清理时生效）"),
    ("db_max_mb", "数据库体积告警阈值 MB（每小时检查时生效）"),
    ("prometheus_url", "Prometheus 地址（留空则自动发现）"),
    (
        "sql_runner_jar",
        "SQL runner jar（application/sessionjob 模式用；留空则只能用 gateway）",
    ),
    (
        "sql_runner_args_style",
        "runner 参数风格：sql-file（默认，配 ConfigMap）或 inline",
    ),
];

pub async fn get(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    let saved = st.store.get_settings().await.unwrap_or_default();

    let effective = json!({
        "audit_retention_days": saved.get("audit_retention_days").cloned()
            .unwrap_or_else(|| st.cfg.audit_retention_days.to_string()),
        "db_max_mb": saved.get("db_max_mb").cloned()
            .unwrap_or_else(|| st.cfg.db_max_mb.to_string()),
        "prometheus_url": saved.get("prometheus_url").cloned()
            .unwrap_or_else(|| st.cfg.prometheus_url.clone().unwrap_or_default()),
        "sql_runner_jar": saved.get("sql_runner_jar").cloned().unwrap_or_default(),
        "sql_runner_args_style": saved.get("sql_runner_args_style").cloned()
            .unwrap_or_else(|| "sql-file".to_string()),
    });

    Ok(Json(json!({
        "effective": effective,
        "saved": saved,
        "keys": KEYS.iter().map(|(k, d)| json!({ "key": k, "hint": d })).collect::<Vec<_>>(),
        "note": "这里只有能热生效的项；bind / webDir / admin 密码改了要重启",
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBody {
    pub audit_retention_days: Option<i64>,
    pub db_max_mb: Option<u64>,
    pub prometheus_url: Option<String>,
    pub sql_runner_jar: Option<String>,
    pub sql_runner_args_style: Option<String>,
}

pub async fn save(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<SaveBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut kv: HashMap<String, String> = HashMap::new();

    if let Some(d) = body.audit_retention_days {
        if !(1..=3650).contains(&d) {
            return Err(ApiError(Error::BadRequest(
                "audit_retention_days 需在 1–3650".into(),
            )));
        }
        kv.insert("audit_retention_days".into(), d.to_string());
    }
    if let Some(m) = body.db_max_mb {
        if !(16..=102_400).contains(&m) {
            return Err(ApiError(Error::BadRequest(
                "db_max_mb 需在 16–102400".into(),
            )));
        }
        kv.insert("db_max_mb".into(), m.to_string());
    }
    if let Some(u) = &body.prometheus_url {
        let u = u.trim();
        if !u.is_empty() && !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err(ApiError(Error::BadRequest(
                "prometheus_url 必须以 http(s):// 开头".into(),
            )));
        }
        kv.insert("prometheus_url".into(), u.to_string());
    }
    if let Some(j) = &body.sql_runner_jar {
        kv.insert("sql_runner_jar".into(), j.trim().to_string());
    }
    if let Some(s) = &body.sql_runner_args_style {
        if s != "inline" && s != "sql-file" {
            return Err(ApiError(Error::BadRequest(
                "sql_runner_args_style 只能是 sql-file 或 inline".into(),
            )));
        }
        kv.insert("sql_runner_args_style".into(), s.clone());
    }
    if kv.is_empty() {
        return Err(ApiError(Error::BadRequest("没有要保存的项".into())));
    }

    st.store.set_settings(&kv).await?;
    // Prometheus 地址变了：让自动发现缓存失效
    *st.prom_cache.write().await = None;
    st.audit(
        &user,
        "settings.save",
        "*",
        "",
        "",
        "ok",
        Some(format!("{:?}", kv.keys().collect::<Vec<_>>())),
        None,
    );
    Ok(Json(json!({ "ok": true, "saved": kv })))
}

pub async fn reset(State(st): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;
    st.store.delete_settings().await?;
    *st.prom_cache.write().await = None;
    st.audit(&user, "settings.reset", "*", "", "", "ok", None, None);
    Ok(Json(
        json!({ "ok": true, "note": "已恢复为环境变量 / 默认值" }),
    ))
}
