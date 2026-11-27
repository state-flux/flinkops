//! 数据源（catalog）解析：把 `catalog.table` 映射到可用连接。

// 子模块通过父模块拿到共享 import 与 helper
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogQuery {
    pub catalog: Option<String>,
    pub database: Option<String>,
    pub table: Option<String>,
}

/// GET /api/sql/catalog/{cluster}/{ns}/{session}?catalog=&database=&table=
///
/// Flink 1.16 的 SQL Gateway 还没有 `/v1/catalogs` REST 端点（404），
/// 所以这里用标准 SQL（SHOW / DESCRIBE）在同一个会话里逐层查。
pub async fn catalog(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path((cluster, ns, session)): axum::extract::Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<CatalogQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let c = st.cluster(&cluster).await?;
    let svc = format!("{session}-sql-gateway");
    let port = c
        .service_node_port(&ns, &svc)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("没找到 SQL Gateway（{svc}）"))))?;
    let host = c.api_host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let gw = SqlGateway::new(format!("http://{host}:{port}"), st.http.clone());

    let gw_key = format!("{cluster}/{ns}/{session}");
    let handle = gw_session(&st, &gw_key, &gw).await?;

    // 在同一个会话里跑一条 SQL，返回第一列字符串
    async fn run_col(
        gw: &SqlGateway,
        handle: &str,
        sql: &str,
    ) -> std::result::Result<Vec<String>, Error> {
        let op = gw.execute(handle, sql).await?;
        let r = gw.fetch(handle, &op, 1000).await?;
        Ok(r.rows
            .iter()
            .filter_map(|row| row.first().and_then(|v| v.as_str()).map(String::from))
            .collect())
    }

    let out: std::result::Result<Value, Error> = async {
        match (
            q.catalog.as_deref(),
            q.database.as_deref(),
            q.table.as_deref(),
        ) {
            (Some(cat), Some(db), Some(tbl)) => {
                let sql = format!("DESCRIBE `{cat}`.`{db}`.`{tbl}`");
                let op = gw.execute(&handle, &sql).await?;
                let r = gw.fetch(&handle, &op, 500).await?;
                Ok(json!({
                    "level": "table",
                    "catalog": cat,
                    "database": db,
                    "table": tbl,
                    "columns": r.columns,
                    "rows": r.rows,
                }))
            }
            (Some(cat), Some(db), None) => {
                let sql = format!("SHOW TABLES FROM `{cat}`.`{db}`");
                let items = run_col(&gw, &handle, &sql).await?;
                Ok(json!({ "level": "tables", "catalog": cat, "database": db, "items": items }))
            }
            (Some(cat), None, _) => {
                // SHOW DATABASES 需要先切到该 catalog
                run_col(&gw, &handle, &format!("USE CATALOG `{cat}`"))
                    .await
                    .ok();
                let items = run_col(&gw, &handle, "SHOW DATABASES").await?;
                Ok(json!({ "level": "databases", "catalog": cat, "items": items }))
            }
            _ => {
                let items = run_col(&gw, &handle, "SHOW CATALOGS").await?;
                Ok(json!({ "level": "catalogs", "items": items }))
            }
        }
    }
    .await;
    let v: Value = out?;
    Ok(Json(v))
}
// ---------------------------------------------------------------- application 模式部署
