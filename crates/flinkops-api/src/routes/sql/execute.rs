//! SQL Gateway：直接执行 / 多语句拆分 / 会话复用 / 版本探测。

// 子模块通过父模块拿到共享 import 与 helper
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteBody {
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    /// 会话集群名（FlinkDeployment，无 job 段）
    pub session: String,
    pub sql: String,
    /// 最多返回多少行（默认 200）
    pub max_rows: Option<usize>,
}

pub(crate) const GATEWAY_MIN: (u32, u32) = (1, 16);

/// gateway 会话空闲多久后重建（秒）
pub(crate) const SQL_SESSION_TTL_SECS: u64 = 600;

/// SQL runner 配置（开源友好：不同用户用不同 jar / 不同参数约定）
pub(crate) struct RunnerSpec {
    pub(crate) jar: String,
    /// "sql-file"（配 ConfigMap + --sql-file）| "inline"（SQL 直接进 args）
    pub(crate) args_style: String,
}

/// 从库配置读 runner，未配置时给出明确指引而不是硬编码默认值。
pub(crate) async fn runner_spec(st: &Arc<AppState>) -> Result<RunnerSpec, Error> {
    let jar = st
        .store
        .get_setting("sql_runner_jar")
        .await
        .filter(|v| !v.trim().is_empty());
    let Some(jar) = jar else {
        return Err(Error::BadRequest(
            "还没配置 SQL runner：请在【设置 → 运行期配置】填 sql_runner_jar（例如 local:///opt/flink/usrlib/sql-runner.jar），\
             或改用 session 模式（走 SQL Gateway，无需 jar）"
                .into(),
        ));
    };
    let args_style = st
        .store
        .get_setting("sql_runner_args_style")
        .await
        .filter(|v| v == "inline" || v == "sql-file")
        .unwrap_or_else(|| "sql-file".to_string());
    Ok(RunnerSpec { jar, args_style })
}

/// 复用 SQL Gateway 会话：Catalog 下钻、连续执行都不用重新握手。
pub(crate) async fn gw_session(
    st: &Arc<AppState>,
    key: &str,
    gw: &SqlGateway,
) -> std::result::Result<String, Error> {
    {
        let cache = st.sql_sessions.read().await;
        if let Some((h, at)) = cache.get(key) {
            if at.elapsed().as_secs() < SQL_SESSION_TTL_SECS {
                return Ok(h.clone());
            }
        }
    }
    let h = gw.open_session().await?;
    st.sql_sessions
        .write()
        .await
        .insert(key.to_string(), (h.clone(), std::time::Instant::now()));
    Ok(h)
}

pub(crate) fn parse_version(v: &str) -> (u32, u32) {
    let clean: String = v
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut it = clean.split('.').filter_map(|x| x.parse::<u32>().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// POST /api/sql/execute
pub async fn execute(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<ExecuteBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if body.sql.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("SQL 不能为空".into())));
    }
    let cluster_id = match body.cluster.clone() {
        Some(c) => c,
        None => st
            .cluster_ids()
            .await
            .into_iter()
            .next()
            .unwrap_or_default(),
    };
    let ns = body.namespace.clone().unwrap_or_else(|| "flink".into());
    let c = st.cluster(&cluster_id).await?;

    // 会话集群校验 + 版本判断（SQL Gateway 从 1.16 才有）
    let obj = c
        .get_raw(flinkops_types::crd::KIND_DEPLOYMENT, &ns, &body.session)
        .await?;
    if obj.get("spec").and_then(|s| s.get("job")).is_some() {
        return Err(ApiError(Error::BadRequest(format!(
            "{} 是 application 模式（有 job 段）；SQL 执行需要 session cluster",
            body.session
        ))));
    }
    let ver = flinkops_types::crd::s(&obj, &["spec", "flinkVersion"]).unwrap_or_default();
    let (major, minor) = parse_version(&ver);
    if (major, minor) < GATEWAY_MIN {
        return Err(ApiError(Error::BadRequest(format!(
            "{}/{} 是 Flink {ver}：SQL Gateway 从 1.16 才有（FLIP-163），当前版本没有 sql-gateway.sh",
            ns, body.session
        ))));
    }

    // 找 gateway Service 的 nodePort
    let svc = format!("{}-sql-gateway", body.session);
    let port = c.service_node_port(&ns, &svc).await?.ok_or_else(|| {
        ApiError(Error::NotFound(format!(
            "没找到 SQL Gateway（{svc}）；先点会话集群的「部署 SQL Gateway」"
        )))
    })?;
    let host = c.api_host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let base = format!("http://{host}:{port}");

    let gw = SqlGateway::new(base.clone(), st.http.clone());
    let started = Instant::now();
    // gateway 1.16 一次只解析一条语句：多语句直接抛 IllegalArgumentException
    // （REST 只剩 "Internal server error."+" "），所以在平台侧拆，逐条在同一 session 里跑，
    // 这样 CREATE TABLE → INSERT 这种脚本才成立。
    let statements = split_statements(body.sql.trim());
    if statements.is_empty() {
        return Err(ApiError(Error::BadRequest("SQL 里没有可执行语句".into())));
    }
    let max_rows = body.max_rows.unwrap_or(200);

    let session = gw.open_session().await?;
    let mut result: Option<flinkops_flink_rest::SqlResult> = None;
    let mut outcomes: Vec<Value> = Vec::with_capacity(statements.len());

    for (idx, stmt) in statements.iter().enumerate() {
        let op = match gw.execute(&session, stmt).await {
            Ok(o) => o,
            Err(e) => {
                let _ = gw.close_session(&session).await;
                return Err(ApiError(with_statement_hint(e, stmt)));
            }
        };
        let r = match gw.fetch(&session, &op, max_rows).await {
            Ok(r) => r,
            Err(e) => {
                let _ = gw.close_session(&session).await;
                return Err(ApiError(with_statement_hint(e, stmt)));
            }
        };
        outcomes.push(json!({
            "index": idx + 1,
            "kind": statement_kind(stmt),
            "rows": r.row_count,
            "jobId": r.job_id.clone(),
        }));
        result = Some(r);
    }
    let _ = gw.close_session(&session).await;

    let mut result = result.unwrap_or_default();
    result.took_ms = started.elapsed().as_millis() as u64;

    st.audit(
        &user,
        "sql.execute",
        &body.session,
        &cluster_id,
        &ns,
        "ok",
        Some(format!(
            "stmts={} rows={} took={}ms",
            statements.len(),
            result.row_count,
            result.took_ms
        )),
        None,
    );

    Ok(Json(json!({
        "ok": true,
        "gateway": base,
        "statements": outcomes,
        "columns": result.columns,
        "rows": result.rows,
        "rowCount": result.row_count,
        "truncated": result.truncated,
        "tookMs": result.took_ms,
        "jobId": result.job_id,
    })))
}

/// 按 `;` 切语句：跳过字符串（' " `）、`--` 行注释与 `/* */` 块注释里的分号。
pub(crate) fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                // '' / "" 是转义，不是语句结束
                if chars.peek() == Some(&q) {
                    cur.push(chars.next().unwrap_or(q));
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                cur.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        cur.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                cur.push(' ');
            }
            ';' => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// 语句类型：用于在前端/审计里区分 DDL、查询、提交作业
pub(crate) fn statement_kind(stmt: &str) -> &'static str {
    let head = stmt
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphabetic())
        .to_ascii_uppercase();
    match head.as_str() {
        "CREATE" | "DROP" | "ALTER" | "USE" | "SET" | "LOAD" => "ddl",
        "INSERT" => "insert",
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" => "query",
        _ => "other",
    }
}

/// gateway 报错常只剩 "Internal server error."，把出错的语句贴回消息里，方便定位
pub(crate) fn with_statement_hint(e: Error, stmt: &str) -> Error {
    let snippet: String = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = if snippet.chars().count() > 120 {
        format!("{}…", snippet.chars().take(120).collect::<String>())
    } else {
        snippet
    };
    match e {
        Error::Upstream { target, message } => Error::Upstream {
            target,
            message: format!("{message}（语句：{snippet}）"),
        },
        Error::Internal(m) => Error::Internal(format!("{m}（语句：{snippet}）")),
        other => other,
    }
}

/// GET /api/sql/gateway/{cluster}/{ns}/{session} —— 探活（/v1/info）
pub async fn gateway_info(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    axum::extract::Path((cluster, ns, session)): axum::extract::Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let c = st.cluster(&cluster).await?;
    let svc = format!("{session}-sql-gateway");
    let port = c.service_node_port(&ns, &svc).await?;
    let Some(port) = port else {
        return Ok(Json(json!({ "found": false, "service": svc })));
    };
    let host = c.api_host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let url = format!("http://{host}:{port}/v1/info");
    let resp = st.http.get(&url).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let info: Value = r.json().await.unwrap_or(Value::Null);
            Ok(Json(
                json!({ "found": true, "up": true, "url": url, "info": info }),
            ))
        }
        Ok(r) => Ok(Json(
            json!({ "found": true, "up": false, "url": url, "status": r.status().as_u16() }),
        )),
        Err(e) => Ok(Json(
            json!({ "found": true, "up": false, "url": url, "error": e.to_string() }),
        )),
    }
}
// ---------------------------------------------------------------- SQL 作业（VVR 式生命周期）
