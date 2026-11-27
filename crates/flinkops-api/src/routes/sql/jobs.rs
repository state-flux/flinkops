//! SQL 作业：CRUD、历史版本、挂起/恢复。

// 子模块通过父模块拿到共享 import 与 helper
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlJobBody {
    pub name: String,
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    pub session: String,
    pub sql_text: String,
}

pub async fn list_jobs(State(st): State<Arc<AppState>>, u: AuthUser) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items = st.store.list_sql_jobs().await?;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/sql/jobs —— 新建 SQL 作业（只存不跑，state=DRAFT）
pub async fn create_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<SqlJobBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if body.name.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("作业名不能为空".into())));
    }
    if body.sql_text.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("SQL 不能为空".into())));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let cluster = body.cluster.unwrap_or_else(|| "local".into());
    let ns = body.namespace.unwrap_or_else(|| "flink".into());
    let job = json!({
        "id": id,
        "name": body.name.trim(),
        "cluster": cluster,
        "namespace": ns,
        "session": body.session,
        "sqlText": body.sql_text,
        "version": 1,
        "state": "DRAFT",
    });
    st.store.upsert_sql_job(&job).await?;
    st.store
        .add_sql_version(&id, 1, &body.sql_text, Some("初始版本"))
        .await?;
    st.audit(
        &user,
        "sql.job.create",
        body.name.trim(),
        &cluster,
        &ns,
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "job": job })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditBody {
    pub sql_text: String,
    pub note: Option<String>,
}

/// PUT /api/sql/jobs/{id} —— 编辑（写新 version，version+1，回 DRAFT）
pub async fn edit_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<EditBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut job = st
        .store
        .get_sql_job(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("SQL 作业 {id} 不存在"))))?;
    if body.sql_text.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("SQL 不能为空".into())));
    }
    let next = job.get("version").and_then(|v| v.as_i64()).unwrap_or(1) + 1;
    job["sqlText"] = json!(body.sql_text);
    job["version"] = json!(next);
    job["state"] = json!("DRAFT");
    st.store.upsert_sql_job(&job).await?;
    st.store
        .add_sql_version(&id, next, &body.sql_text, body.note.as_deref())
        .await?;
    st.audit(
        &user,
        "sql.job.edit",
        &id,
        "",
        "",
        "ok",
        Some(format!("v{next}")),
        None,
    );
    Ok(Json(json!({ "ok": true, "job": job })))
}

/// GET /api/sql/jobs/{id}/versions
pub async fn versions(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let items = st.store.list_sql_versions(&id).await?;
    Ok(Json(json!({ "items": items })))
}

/// DELETE /api/sql/jobs/{id}
pub async fn delete_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.delete_sql_job(&id).await?;
    st.audit(&user, "sql.job.delete", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}

/// 复用一个内部函数：把 SQL 丢给 gateway（execute 接口里也用它）
pub(crate) async fn run_on_gateway(
    st: &Arc<AppState>,
    cluster: &str,
    ns: &str,
    session: &str,
    sql_text: &str,
) -> Result<Value, Error> {
    let c = st.cluster(cluster).await?;
    let svc = format!("{session}-sql-gateway");
    let port = c
        .service_node_port(ns, &svc)
        .await?
        .ok_or_else(|| Error::NotFound(format!("没找到 SQL Gateway（{svc}）")))?;
    let host = c.api_host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let gw = SqlGateway::new(format!("http://{host}:{port}"), st.http.clone());

    let handle = gw.open_session().await?;
    let op = match gw.execute(&handle, sql_text).await {
        Ok(o) => o,
        Err(e) => {
            let _ = gw.close_session(&handle).await;
            return Err(e);
        }
    };
    let fetched = gw.fetch(&handle, &op, 200).await;
    let _ = gw.close_session(&handle).await;
    let r = fetched?;
    Ok(json!({
        "columns": r.columns,
        "rows": r.rows,
        "rowCount": r.row_count,
        "tookMs": r.took_ms,
    }))
}
// ---------------------------------------------------------------- 生命周期的暂停 / 恢复 / 回滚 / 升级

/// POST /api/sql/jobs/{id}/suspend —— 取消运行中的作业
pub async fn suspend_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut job = st
        .store
        .get_sql_job(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("SQL 作业 {id} 不存在"))))?;

    let cluster = job
        .get("cluster")
        .and_then(|v| v.as_str())
        .unwrap_or("local")
        .to_string();
    let ns = job
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("flink")
        .to_string();
    let session = job
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let job_id = job.get("jobId").and_then(|v| v.as_str()).map(String::from);

    let Some(job_id) = job_id else {
        job["state"] = json!("DRAFT");
        st.store.upsert_sql_job(&job).await?;
        return Ok(Json(
            json!({ "ok": true, "job": job, "note": "没有 jobId（可能是 SELECT 类），已置回 DRAFT" }),
        ));
    };

    let rest = session_rest(&st, &cluster, &ns, &session).await?;
    rest.cancel_job(&job_id).await?;
    job["state"] = json!("SUSPENDED");
    st.store.upsert_sql_job(&job).await?;
    st.audit(
        &user,
        "sql.job.suspend",
        &id,
        &cluster,
        &ns,
        "ok",
        Some(job_id),
        None,
    );
    Ok(Json(json!({ "ok": true, "job": job })))
}

/// POST /api/sql/jobs/{id}/resume —— 重新提交（等价于 deploy 当前版本）
pub async fn resume_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;

    deploy_inner(st, headers, id, DeployOpts::default()).await
}
