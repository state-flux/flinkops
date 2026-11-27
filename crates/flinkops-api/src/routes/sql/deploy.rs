//! 由 SQL 作业生成并下发作业（FlinkDeployment / FlinkSessionJob / 应用模板）。

// 子模块通过父模块拿到共享 import 与 helper
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployOpts {
    /// 部署哪个版本（不填=当前版本）
    pub version: Option<i64>,
    /// 升级前先打 savepoint 存档（默认 true）
    #[serde(default)]
    pub savepoint: bool,
    /// session（默认，走 gateway）| application（FlinkDeployment + sql-runner）
    pub mode: Option<String>,
    /// application 模式：runner jar 路径
    pub runner_jar: Option<String>,
    /// application 模式：部署名
    pub deployment_name: Option<String>,
    /// application 模式：覆盖镜像（默认沿用会话集群的镜像）
    pub image: Option<String>,
    /// application 模式：用哪个部署模板（默认 builtin-sql-app）
    pub template: Option<String>,
}

/// 找到 session cluster 的 JM REST（SQL 作业跑在它上面）
pub(crate) async fn session_rest(
    st: &Arc<AppState>,
    cluster: &str,
    ns: &str,
    session: &str,
) -> Result<flinkops_flink_rest::FlinkRest, Error> {
    st.rest_for(cluster, ns, session).await
}

/// POST /api/sql/jobs/{id}/deploy —— 部署/升级；可指定版本、可先打 savepoint
pub async fn deploy_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<DeployOpts>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;

    let opts = body.map(|j| j.0).unwrap_or_default();
    deploy_inner(st, headers, id, opts).await
}

pub(crate) async fn deploy_inner(
    st: Arc<AppState>,
    headers: HeaderMap,
    id: String,
    opts: DeployOpts,
) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;
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

    // 回滚：从版本历史里取指定版本的 SQL
    let (sql_text, used_version) = match opts.version {
        Some(v) => {
            let versions = st.store.list_sql_versions(&id).await?;
            let found = versions
                .iter()
                .find(|x| x.get("version").and_then(|n| n.as_i64()) == Some(v))
                .cloned()
                .ok_or_else(|| ApiError(Error::BadRequest(format!("版本 v{v} 不存在"))))?;
            (
                found
                    .get("sqlText")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                v,
            )
        }
        None => (
            job.get("sqlText")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            job.get("version").and_then(|x| x.as_i64()).unwrap_or(1),
        ),
    };

    // 升级前存档：先打 savepoint，再 cancel 旧作业
    let mut saved_to: Option<String> = None;
    let running_job = job.get("jobId").and_then(|v| v.as_str()).map(String::from);
    if let (true, Some(job_id)) = (opts.savepoint, running_job.clone()) {
        if let Ok(rest) = session_rest(&st, &cluster, &ns, &session).await {
            let dir = job
                .get("namespace")
                .map(|_| format!("file:///tmp/flink-savepoints/{id}"));
            if let Ok(resp) = rest.trigger_savepoint(&job_id, dir.as_deref()).await {
                if let Some(trigger) = resp.get("request-id").and_then(|v| v.as_str()) {
                    match rest.wait_savepoint(&job_id, trigger, 120).await {
                        Ok(Some(path)) => saved_to = Some(path),
                        Ok(None) => {}
                        Err(_) => {}
                    }
                }
            }
            let _ = rest.cancel_job(&job_id).await;
        }
    } else if let Some(job_id) = running_job.clone() {
        // 不允许并存：重启前先取消旧实例
        if let Ok(rest) = session_rest(&st, &cluster, &ns, &session).await {
            let _ = rest.cancel_job(&job_id).await;
        }
    }

    match run_on_gateway(&st, &cluster, &ns, &session, &sql_text).await {
        Ok(r) => {
            let job_id = r.get("jobId").and_then(|v| v.as_str()).map(String::from);
            // INSERT 类有 jobId（常驻作业），SELECT 类没有（一次性查询）；两者都算部署成功
            job["state"] = json!("RUNNING");
            job["jobId"] = job_id.as_deref().map(|v| json!(v)).unwrap_or(Value::Null);
            job["sqlText"] = json!(sql_text);
            job["version"] = json!(used_version);
            job["lastError"] = Value::Null;
            st.store.upsert_sql_job(&job).await?;
            st.audit(
                &user,
                "sql.job.deploy",
                &id,
                &cluster,
                &ns,
                "ok",
                Some(format!(
                    "v{used_version} savepoint={saved_to:?} job={job_id:?}"
                )),
                None,
            );
            Ok(Json(json!({
                "ok": true,
                "job": job,
                "version": used_version,
                "savepoint": saved_to,
                "result": r,
            })))
        }
        Err(e) => {
            job["state"] = json!("FAILED");
            job["lastError"] = json!(e.to_string());
            let _ = st.store.upsert_sql_job(&job).await;
            st.audit(
                &user,
                "sql.job.deploy",
                &id,
                &cluster,
                &ns,
                "fail",
                Some(e.to_string()),
                None,
            );
            Ok(Json(
                json!({ "ok": false, "job": job, "error": e.to_string() }),
            ))
        }
    }
}

// ---------------------------------------------------------------- Catalog 浏览

/// 渲染 `builtin-sql-app` 模板要用的变量。值是「已按 YAML 语义格式化好的字符串」：
/// 字符串自带双引号，数组是 JSON，数字裸写（见 deploy_templates::render 的约定）。
pub(crate) struct DeployAppVars<'a> {
    pub(crate) name: &'a str,
    pub(crate) ns: &'a str,
    pub(crate) image: &'a str,
    pub(crate) flink_version: &'a str,
    pub(crate) service_account: &'a str,
    pub(crate) slots: &'a str,
    pub(crate) runner: &'a str,
    pub(crate) args: &'a Value,
    pub(crate) parallelism: i64,
    pub(crate) upgrade_mode: &'a str,
    pub(crate) sql_config_map: &'a str,
}

/// 模板里的 `{{x}}` 有两种位置：
/// - 被引号包着（`name: "{{name}}"`）→ 这里给**裸文本**，只转义引号与反斜杠
/// - 裸写（`replicas: {{tmReplicas}}`、`args: {{args}}`）→ 给 YAML 字面量（数字 / JSON 数组）
pub(crate) fn yaml_text(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn deploy_app_vars(v: DeployAppVars<'_>) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    m.insert("name".into(), yaml_text(v.name));
    m.insert("namespace".into(), yaml_text(v.ns));
    m.insert("image".into(), yaml_text(v.image));
    m.insert("flinkVersion".into(), yaml_text(v.flink_version));
    m.insert("serviceAccount".into(), yaml_text(v.service_account));
    m.insert("slots".into(), yaml_text(v.slots));
    m.insert("restartStrategy".into(), yaml_text("none"));
    m.insert(
        "checkpointDir".into(),
        yaml_text(&format!("file:///tmp/flink-checkpoints/{}", v.name)),
    );
    m.insert(
        "savepointDir".into(),
        yaml_text(&format!("file:///tmp/flink-savepoints/{}", v.name)),
    );
    m.insert("jmCpu".into(), yaml_text("1"));
    m.insert("jmMemory".into(), yaml_text("2Gi"));
    m.insert("tmCpu".into(), yaml_text("1"));
    m.insert("tmMemory".into(), yaml_text("2Gi"));
    m.insert("tmReplicas".into(), "1".into());
    m.insert("runnerJar".into(), yaml_text(v.runner));
    m.insert(
        "args".into(),
        serde_json::to_string(v.args).unwrap_or_else(|_| "[]".into()),
    );
    m.insert("parallelism".into(), v.parallelism.to_string());
    m.insert("upgradeMode".into(), yaml_text(v.upgrade_mode));
    m.insert("sqlConfigMap".into(), yaml_text(v.sql_config_map));
    // SessionJob 模板用得到，一起给，避免自定义模板引用不到
    m.insert("sessionCluster".into(), yaml_text(""));
    m
}

/// POST /api/sql/jobs/{id}/deploy-as-app
///
/// operator 官方的 SQL application 做法（examples/sql-example.yaml）：
/// FlinkDeployment + jarURI=sql-runner.jar + args=[SQL 脚本路径]，
/// 脚本通过 ConfigMap 挂到 /opt/flink/usrlib/sql-scripts。
pub async fn deploy_as_app(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<DeployOpts>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut job = st
        .store
        .get_sql_job(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("SQL 作业 {id} 不存在"))))?;
    let opts = body.map(|j| j.0).unwrap_or_default();

    let cluster_id = job
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
    let sql_text = match opts.version {
        Some(v) => {
            let versions = st.store.list_sql_versions(&id).await?;
            versions
                .iter()
                .find(|x| x.get("version").and_then(|n| n.as_i64()) == Some(v))
                .and_then(|x| x.get("sqlText").and_then(|t| t.as_str()).map(String::from))
                .ok_or_else(|| ApiError(Error::BadRequest(format!("版本 v{v} 不存在"))))?
        }
        None => job
            .get("sqlText")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };

    let c = st.cluster(&cluster_id).await?;
    let src = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &session).await?;
    let image = opts
        .image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| crd::s(&src, &["spec", "image"]))
        .ok_or_else(|| ApiError(Error::BadRequest("会话集群没有 image".into())))?;
    let version = crd::s(&src, &["spec", "flinkVersion"]).unwrap_or_else(|| "v1_16".into());
    let sa = crd::s(&src, &["spec", "serviceAccount"]).unwrap_or_else(|| "flink".into());
    let slots = crd::s(
        &src,
        &[
            "spec",
            "flinkConfiguration",
            "taskmanager.numberOfTaskSlots",
        ],
    )
    .unwrap_or_else(|| "2".into());

    let spec_runner = runner_spec(&st).await.map_err(ApiError)?;
    let runner = opts.runner_jar.clone().unwrap_or(spec_runner.jar.clone());
    let args = match spec_runner.args_style.as_str() {
        "inline" => json!(["--sql", sql_text.clone()]),
        _ => json!(["--sql-file", "/opt/flink/usrlib/sql-scripts/job.sql"]),
    };
    let raw_name = opts.deployment_name.clone().unwrap_or_else(|| {
        format!(
            "{}-app",
            job.get("name").and_then(|v| v.as_str()).unwrap_or("sql")
        )
    });
    let dep_name: String = raw_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();
    let cm_name = format!("{dep_name}-sql");

    let mut data = std::collections::BTreeMap::new();
    data.insert("job.sql".to_string(), sql_text.clone());
    c.apply_configmap(&ns, &cm_name, data).await?;

    // 产出 CR 的 YAML 走模板（内置 builtin-sql-app，可在「部署模板」页另存自定义版本）
    let tpl = crate::routes::deploy_templates::resolve_template(&st, opts.template.as_deref())
        .await
        .map_err(ApiError)?;
    let vars = deploy_app_vars(DeployAppVars {
        name: &dep_name,
        ns: &ns,
        image: &image,
        flink_version: &version,
        service_account: &sa,
        slots: &slots,
        runner: &runner,
        args: &args,
        parallelism: 1,
        upgrade_mode: if opts.savepoint {
            "savepoint"
        } else {
            "stateless"
        },
        sql_config_map: &cm_name,
    });
    let rendered = crate::routes::deploy_templates::render(&tpl, &vars);
    let missing = crate::routes::deploy_templates::unresolved(&rendered);
    if !missing.is_empty() {
        return Err(ApiError(Error::BadRequest(format!(
            "部署模板里还有未替换的占位符：{}（可用占位符见 GET /api/deploy-templates）",
            missing.join(", ")
        ))));
    }
    let mut obj: Value = serde_yaml::from_str(&rendered)
        .map_err(|e| ApiError(Error::BadRequest(format!("模板渲染结果不是合法 YAML：{e}"))))?;
    // 模板管结构，归属标签由平台补：否则「平台管理的作业」筛选看不到它
    if let Some(meta) = obj.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        let labels = meta
            .entry("labels")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| ApiError(Error::BadRequest("模板 metadata.labels 必须是 map".into())))?;
        labels.insert("flinkops.io/managed-by".into(), json!("flinkops"));
        labels.insert("flinkops.io/sql-job".into(), json!(id.clone()));
    }

    c.apply_custom(
        "flink.apache.org",
        "v1beta1",
        crd::KIND_DEPLOYMENT,
        &ns,
        obj,
    )
    .await?;

    job["state"] = json!("SUBMITTED");
    job["jobId"] = Value::Null;
    job["lastError"] = Value::Null;
    st.store.upsert_sql_job(&job).await?;
    st.audit(
        &user,
        "sql.job.deploy-app",
        &id,
        &cluster_id,
        &ns,
        "ok",
        Some(format!(
            "deployment={dep_name} cm={cm_name} runner={runner}"
        )),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "mode": "application",
        "deployment": dep_name,
        "configMap": cm_name,
        "runnerJar": runner,
        "job": job,
        "note": "镜像里必须有 sql-runner.jar；没有就先用 session 模式（gateway）",
    })))
}

/// POST /api/sql/jobs/{id}/deploy-as-sessionjob
///
/// 把 SQL 脚本作为 FlinkSessionJob 提交到会话集群：
/// 需要一个能执行 SQL 的 runner jar（args 传 SQL 文件路径），
/// SQL 文本走 ConfigMap 挂载。相比 gateway 提交，它有 CRD 生命周期与权威 jobId。
pub async fn deploy_as_sessionjob(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<DeployOpts>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut job = st
        .store
        .get_sql_job(&id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("SQL 作业 {id} 不存在"))))?;
    let opts = body.map(|j| j.0).unwrap_or_default();

    let cluster_id = job
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
    let sql_text = match opts.version {
        Some(v) => {
            let versions = st.store.list_sql_versions(&id).await?;
            versions
                .iter()
                .find(|x| x.get("version").and_then(|n| n.as_i64()) == Some(v))
                .and_then(|x| x.get("sqlText").and_then(|t| t.as_str()).map(String::from))
                .ok_or_else(|| ApiError(Error::BadRequest(format!("版本 v{v} 不存在"))))?
        }
        None => job
            .get("sqlText")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };

    let c = st.cluster(&cluster_id).await?;
    let raw_name = opts.deployment_name.clone().unwrap_or_else(|| {
        format!(
            "{}-sj",
            job.get("name").and_then(|v| v.as_str()).unwrap_or("sql")
        )
    });
    let sj_name: String = raw_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();
    let cm_name = format!("{sj_name}-sql");

    // SQL → ConfigMap
    let mut data = std::collections::BTreeMap::new();
    data.insert("job.sql".to_string(), sql_text.clone());
    c.apply_configmap(&ns, &cm_name, data).await?;

    let spec_runner = runner_spec(&st).await.map_err(ApiError)?;
    let runner = opts.runner_jar.clone().unwrap_or(spec_runner.jar.clone());
    let args = match spec_runner.args_style.as_str() {
        "inline" => json!(["--sql", sql_text.clone()]),
        _ => json!(["--sql-file", "/opt/flink/usrlib/sql-scripts/job.sql"]),
    };

    // 注意：SessionJob 的 podTemplate 由 session cluster 决定，
    // 所以 ConfigMap 必须挂到 session cluster 上（这里只做占位提示）。
    let obj = json!({
        "apiVersion": "flink.apache.org/v1beta1",
        "kind": crd::KIND_SESSION_JOB,
        "metadata": {
            "name": sj_name,
            "namespace": ns,
            "labels": { "flinkops.io/managed-by": "flinkops", "flinkops.io/sql-job": id }
        },
        "spec": {
            "deploymentName": session,
            "job": {
                "jarURI": runner,
                "args": args,
                "parallelism": 1,
                "upgradeMode": if opts.savepoint { "savepoint" } else { "stateless" }
            }
        }
    });
    c.apply_custom(
        "flink.apache.org",
        "v1beta1",
        crd::KIND_SESSION_JOB,
        &ns,
        obj,
    )
    .await?;

    job["state"] = json!("SUBMITTED");
    job["lastError"] = Value::Null;
    st.store.upsert_sql_job(&job).await?;
    st.audit(
        &user,
        "sql.job.deploy-sessionjob",
        &id,
        &cluster_id,
        &ns,
        "ok",
        Some(format!("sessionjob={sj_name} cm={cm_name}")),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "mode": "sessionjob",
        "sessionJob": sj_name,
        "configMap": cm_name,
        "runnerJar": runner,
        "job": job,
        "note": "SQL 脚本挂在 ConfigMap；会话集群需要能挂到这个 ConfigMap（podTemplate）且镜像里有 sql-runner.jar",
    })))
}
