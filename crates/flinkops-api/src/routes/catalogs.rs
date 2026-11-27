//! 外部数据源元数据：注册 DSN → 从 information_schema 拉库表结构 → 存本地供补全。
//!
//! 走平台自己的 MySQL 协议连接（sqlx），不依赖 Flink 的 JDBC catalog——
//! 那个需要镜像里有 flink-connector-jdbc，而当前镜像是没有的。
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

pub async fn list(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    let items = st.store.list_catalogs().await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterBody {
    pub id: Option<String>,
    pub name: String,
    /// starrocks | mysql | doris
    pub kind: Option<String>,
    /// 例：mysql://user:pass@mysql.example.com:3306/analytics
    pub dsn: String,
}

pub async fn register(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<RegisterBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if body.name.trim().is_empty() || body.dsn.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("name 与 dsn 都不能为空".into())));
    }
    let id = body.id.clone().unwrap_or_else(|| {
        body.name
            .trim()
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric(), "-")
    });
    let kind = body.kind.unwrap_or_else(|| "starrocks".into());
    st.store
        .save_catalog(&id, body.name.trim(), &kind, body.dsn.trim())
        .await?;
    st.audit(&user, "catalog.register", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.delete_catalog(&id).await?;
    st.audit(&user, "catalog.delete", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}

/// POST /api/catalogs/{id}/sync —— 拉 information_schema，存本地
pub async fn sync(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let (_name, dsn) = st
        .store
        .catalog_dsn(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("数据源 {id} 不存在"))))?;

    let tables = flinkops_store::Store::fetch_external_schema(&dsn)
        .await
        .map_err(ApiError)?;
    let count = st.store.replace_catalog_tables(&id, &tables).await?;

    st.audit(
        &user,
        "catalog.sync",
        &id,
        "",
        "",
        "ok",
        Some(format!("tables={count}")),
        None,
    );
    Ok(Json(json!({ "ok": true, "tables": count })))
}

pub async fn tables(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items = st.store.list_catalog_tables(&id).await?;
    Ok(Json(json!({ "items": items })))
}

/// GET /api/catalogs/complete?prefix=tik — 补全候选（表 + 列）
pub async fn complete(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let prefix = q.get("prefix").cloned().unwrap_or_default().to_lowercase();
    let catalogs = st.store.list_catalogs().await?;
    let mut out = Vec::new();
    for c in &catalogs {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let cname = c.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        for t in st.store.list_catalog_tables(id).await? {
            let db = t.get("db").and_then(|v| v.as_str()).unwrap_or_default();
            let tbl = t.get("table").and_then(|v| v.as_str()).unwrap_or_default();
            let full = format!("{cname}.{db}.{tbl}");
            if prefix.is_empty() || full.to_lowercase().contains(&prefix) {
                out.push(json!({
                    "label": full,
                    "kind": "table",
                    "columns": t.get("columns").cloned().unwrap_or(json!([])),
                }));
            }
        }
    }
    Ok(Json(
        json!({ "items": out.into_iter().take(200).collect::<Vec<_>>() }),
    ))
}
/// GET /api/catalogs/{id}/diff —— 对比「平台缓存」与「数据源实时结构」
///
/// 用途：改完表之后检测变更（新增表 / 删除表 / 列增删 / 类型变更），
/// 也是迁移前对账的基础。
pub async fn diff(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let (_name, dsn) = st
        .store
        .catalog_dsn(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("数据源 {id} 不存在"))))?;

    // 实时（全量，202 张表约 3s）
    let live = flinkops_store::Store::fetch_external_schema(&dsn)
        .await
        .map_err(ApiError)?;
    // 缓存
    let cached = st.store.list_catalog_tables(&id).await?;

    use std::collections::BTreeMap;
    let mut live_map: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for (db, table, cols) in live {
        let arr = cols.as_array().cloned().unwrap_or_default();
        live_map.insert((db, table), arr);
    }
    let mut cache_map: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for t in &cached {
        let db = t
            .get("db")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let table = t
            .get("table")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let arr = t
            .get("columns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        cache_map.insert((db, table), arr);
    }

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (key, live_cols) in &live_map {
        match cache_map.get(key) {
            None => added.push(json!({ "db": key.0, "table": key.1, "columns": live_cols.len() })),
            Some(cache_cols) => {
                // 按列名建索引
                let idx = |cols: &[Value]| {
                    let mut m: BTreeMap<String, String> = BTreeMap::new();
                    for c in cols {
                        let n = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let t = c
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        m.insert(n, t);
                    }
                    m
                };
                let a = idx(live_cols);
                let b = idx(cache_cols);
                let mut cols_added = Vec::new();
                let mut cols_removed = Vec::new();
                let mut retyped = Vec::new();
                for (n, t) in &a {
                    match b.get(n) {
                        None => cols_added.push(json!({ "name": n, "type": t })),
                        Some(old) if old != t => {
                            retyped.push(json!({ "name": n, "from": old, "to": t }))
                        }
                        _ => {}
                    }
                }
                for (n, t) in &b {
                    if !a.contains_key(n) {
                        cols_removed.push(json!({ "name": n, "type": t }));
                    }
                }
                if !cols_added.is_empty() || !cols_removed.is_empty() || !retyped.is_empty() {
                    changed.push(json!({
                        "db": key.0,
                        "table": key.1,
                        "added": cols_added,
                        "removed": cols_removed,
                        "retyped": retyped,
                    }));
                }
            }
        }
    }
    for key in cache_map.keys() {
        if !live_map.contains_key(key) {
            removed.push(json!({ "db": key.0, "table": key.1 }));
        }
    }

    Ok(Json(json!({
        "ok": true,
        "liveTables": live_map.len(),
        "cachedTables": cache_map.len(),
        "added": added,
        "removed": removed,
        "changed": changed,
    })))
}
