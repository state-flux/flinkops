//! 作业读路径：列表 / 详情 / 指标 / 日志 / 事件 / 顶点诊断（背压、火焰图、profile）。

// 子模块通过父模块拿到全部共享 import 与 helper（child 可见父的私有项）
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    #[serde(default)]
    pub refresh: bool,
    /// 分页：limit 默认 5000（上限 20000），offset 从 0 开始
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    if q.refresh {
        sync_clusters(&st, q.cluster.as_deref(), q.namespace.as_deref(), true).await?;
    }
    // ns 过滤与分页下推到 SQL：集群上千作业时不再整表读进内存
    let (rows, total) = st
        .store
        .list_jobs_page(
            q.cluster.as_deref(),
            q.namespace.as_deref(),
            q.limit,
            q.offset.unwrap_or(0).max(0),
        )
        .await?;
    // MCP Token 的命名空间/作业白名单对集合接口同样生效：
    // 否则"限定 team-a 的只读 Token"能枚举全集群作业（含 jarURI / savepoint 路径）。
    let tok = crate::mcp_guard::peek(&st, &headers).await?;
    // 多租户：非 admin 只看得到白名单 namespace 内的作业（与写权限同一套口径）。
    // MCP Bearer 请求没有 Cookie 会话 → principal 为 None，此时不能当成"无权"，
    // 否则机器人调用列表永远是空的；它的可见范围由 token 的 allowNamespaces/allowJobs 决定。
    let principal = st.principal(&headers).await;
    let multi_tenant = principal.as_ref().map(|p| !p.is_admin()).unwrap_or(false);
    let items: Vec<Value> = rows
        .into_iter()
        .filter(|j| {
            principal
                .as_ref()
                .map(|p| p.ns_ok(&j.namespace))
                .unwrap_or(true)
        })
        .filter(|j| {
            tok.as_ref()
                .map(|t| crate::mcp_guard::job_allowed(t, &j.namespace, &j.name))
                .unwrap_or(true)
        })
        .map(|j| {
            let health = core::health(&j);
            let mut v = serde_json::to_value(&j).unwrap_or(Value::Null);
            v["health"] = json!(health);
            // 实际并行度：只有详情页算过才有值（缓存 10 分钟），不会为此打 51 次 JM
            if let Some(actual) =
                st.actual_parallelism(&format!("{}/{}/{}", j.cluster, j.namespace, j.name))
            {
                v["parallelismActual"] = json!(actual);
            }
            v
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "count": items.len(),
        // 分页在 SQL 里做、ns 过滤在内存里做，两者一叠加 total 就会泄露
        // 未授权 namespace 的作业总数；发生过内存过滤时以过滤后的条数为准。
        "total": if multi_tenant || tok.is_some() { items.len() as i64 } else { total },
        "limit": q.limit.unwrap_or(5000),
        "offset": q.offset.unwrap_or(0),
        // 数据新鲜度：列表来自 SQL 索引，详情来自实时 CRD。
        // 前端据此显示"索引滞后 Ns"，避免"列表 RUNNING / 详情 RECONCILING"被当成 bug。
        "asOf": now_rfc3339(),
        "indexAgeSecs": st.index_age_secs(),
    })))
}

/// 主动同步（前端“刷新”按钮 / 定时任务）。
pub async fn sync(State(st): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;
    // 只读平台不该改写本地索引（它同样是"写"）
    if st.cfg.readonly {
        return Err(ApiError(Error::Forbidden("平台处于只读模式".into())));
    }
    let n = sync_clusters(&st, None, None, true).await?;
    st.audit(
        &user,
        "jobs.sync",
        "*",
        "",
        "",
        "ok",
        Some(format!("{n} jobs")),
        None,
    );
    Ok(Json(json!({ "ok": true, "synced": n })))
}

/// 从各集群拉 CRD 并写入本地索引。单个集群/作业失败只告警，不影响其它。
///
/// `wait=false` 用于后台周期任务：拿不到锁就跳过本轮（两次全量覆盖会互相删行）；
/// `wait=true` 用于手动刷新，排队等前一轮结束。
pub async fn sync_clusters(
    st: &Arc<AppState>,
    cluster: Option<&str>,
    ns: Option<&str>,
    wait: bool,
) -> Result<usize, ApiError> {
    let _guard = if wait {
        st.sync_lock.lock().await
    } else {
        match st.sync_lock.try_lock() {
            Ok(g) => g,
            Err(_) => return Ok(0),
        }
    };
    let mut n = 0usize;
    for (id, c) in &st.cluster_entries().await {
        if let Some(want) = cluster {
            if want != id {
                continue;
            }
        }
        match c.list_jobs(ns).await {
            Ok(jobs) => {
                // 全量替换跑在**一个事务**里：读者不会看到"删完了、还没写回"的空窗，
                // 并发刷新也不会互相删行；带 ns 的刷新同样会清理该 namespace 的旧行。
                match st.store.replace_cluster_jobs(id, ns, &jobs).await {
                    Ok((deleted, written)) => {
                        n += written;
                        tracing::debug!(cluster = %id, deleted, written, "job index replaced");
                        st.mark_index_fresh();
                    }
                    Err(e) => warn!(cluster = %id, error = %e, "index replace failed"),
                }
            }
            Err(e) => warn!(cluster = %id, error = %e, "sync cluster failed"),
        }
    }
    // 清理孤儿：索引里有、但已经不在"平台认定的集群集合"里的（比如删掉的动态集群）。
    // 判据必须是 configured_cluster_ids（env ∪ 库 ∪ 已连上），
    // 不能用 cluster_ids()：那只是"本次连上的"，集群临时不可达会被误清空。
    if ns.is_none() && cluster.is_none() {
        let known: Vec<String> = st.configured_cluster_ids().await;
        if let Ok(all) = st.store.list_job_clusters().await {
            for c in all {
                if !known.contains(&c) {
                    match st.store.delete_jobs_by_cluster(&c).await {
                        Ok(k) if k > 0 => {
                            warn!(cluster = %c, pruned = k, "orphan job index purged")
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------- 详情

pub async fn detail(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
) -> ApiResult<Json<JobDetail>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let c = st.cluster(&cluster).await?;
    // 列表里既有 FlinkDeployment 也有 FlinkSessionJob，详情必须都能打开
    let (_kind, obj) = c.get_any(&ns, &name).await?;
    let mut d = crd::detail(&cluster, &obj);

    // checkpoint 计数 CRD 里没有 → 用 REST 补；失败保持 0，但 detail 仍然可用
    if let Some(job_id) = d.summary.job_id.clone() {
        // 注意：作业被挂起后 JM Service 会被删，`rest_for` 会直接失败。
        // 这时只能降级 checkpoint 区块，**不能让整个详情 502** ——
        // 否则页面上并行度/镜像/JAR/upgradeMode 全变 "—"（实测挂起后即如此）。
        match st.rest_for(&cluster, &ns, &name).await {
            Ok(rest) => {
                match rest.checkpoints(&job_id).await {
                    Ok(v) => {
                        if let Some(counts) = v.get("counts") {
                            d.checkpoint.completed = counts
                                .get("completed")
                                .and_then(|x| x.as_i64())
                                .unwrap_or(0);
                            d.checkpoint.failed =
                                counts.get("failed").and_then(|x| x.as_i64()).unwrap_or(0);
                            d.checkpoint.restored =
                                counts.get("restored").and_then(|x| x.as_i64()).unwrap_or(0);
                            d.checkpoint.in_progress = counts
                                .get("in_progress")
                                .and_then(|x| x.as_i64())
                                .unwrap_or(0);
                        }
                    }
                    Err(e) => warn!(job = %name, error = %e, "checkpoint stats unavailable"),
                }
                // 实际并行度：spec 只写了入口算子，job 图才是真的（6 TM × 4 slots = 24）
                if let Ok(job) = rest.job(&job_id).await {
                    let actual = job
                        .get("vertices")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.get("parallelism").and_then(|p| p.as_i64()))
                                .max()
                                .unwrap_or(0)
                        })
                        .unwrap_or(0);
                    if actual > 0 {
                        d.summary.parallelism_actual = Some(actual);
                        // 列表页也能看到（缓存 10 分钟，避免列表挨个打 JM）
                        st.put_actual_parallelism(&format!("{cluster}/{ns}/{name}"), actual);
                    }
                }
            }
            Err(e) => warn!(
                job = %name,
                error = %e,
                "jobmanager REST 不可达（作业可能已挂起）；详情仅返回 CRD 部分"
            ),
        }
    }
    Ok(Json(d))
}

// ---------------------------------------------------------------- 实时指标

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsQuery {
    /// 逗号分隔；缺省取 LIVE_METRICS（批量，避免全量扫描）
    pub ids: Option<String>,
}

pub async fn metrics(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<LiveMetrics>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let c = st.cluster(&cluster).await?;
    let (_kind, obj) = c.get_any(&ns, &name).await?;
    let job_id = crd::s(&obj, &["status", "jobStatus", "jobId"])
        .ok_or_else(|| Error::Conflict(format!("{name} 还没有 JobID（未提交或正在调谐）")))?;

    let ids: Vec<&str> = match &q.ids {
        Some(s) if !s.trim().is_empty() => s.split(',').map(|x| x.trim()).collect(),
        _ => LIVE_METRICS.to_vec(),
    };

    let rest = st.rest_for(&cluster, &ns, &name).await?;

    // 1) job 级指标（checkpoint 统计、uptime…）
    let mut values = match rest.metrics(&job_id, &ids).await {
        Ok(v) => v,
        Err(e) => {
            let has_prom = st.cfg.prometheus_url.is_some();
            return Ok(Json(LiveMetrics {
                job_id,
                values: Default::default(),
                source: if has_prom { "prometheus" } else { "none" }.into(),
                sampled_at: now_rfc3339(),
                degraded: Some(e.to_string()),
            }));
        }
    };

    // 2) 吞吐：job 级没有，按 vertex × subtask 聚合
    //    `rate_*` 直接来自 Flink 的 `*PerSecond`（1.15 起只有 subtask 聚合可取），
    //    拿不到（vertices=0，作业图还没起来）才退回"两次采样差"。
    if let Ok(t) = rest.job_throughput(&job_id).await {
        values.insert("numRecordsIn".into(), t.records_in);
        values.insert("numRecordsOut".into(), t.records_out);
        values.insert("vertices".into(), t.vertices as f64);

        let now = Instant::now();
        let mut cache = st.throughput_cache.write().await;
        if t.vertices > 0 {
            values.insert("numRecordsInPerSecond".into(), t.rate_in);
            values.insert("numRecordsOutPerSecond".into(), t.rate_out);
        } else if let Some((pi, po, at)) = cache.get(&job_id).copied() {
            let dt = at.elapsed().as_secs_f64();
            if dt >= 1.0 {
                values.insert(
                    "numRecordsInPerSecond".into(),
                    ((t.records_in - pi) / dt).max(0.0),
                );
                values.insert(
                    "numRecordsOutPerSecond".into(),
                    ((t.records_out - po) / dt).max(0.0),
                );
            }
        }
        cache.insert(job_id.clone(), (t.records_in, t.records_out, now));
    }

    Ok(Json(LiveMetrics {
        job_id,
        values,
        source: "flink-rest".into(),
        sampled_at: now_rfc3339(),
        degraded: None,
    }))
}

// ---------------------------------------------------------------- 日志

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogQuery {
    pub kind: Option<String>,
    /// 只取最后 N 行（默认 2000，上限 20000）
    pub tail: Option<i64>,
}

pub async fn log(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
    Query(q): Query<LogQuery>,
) -> ApiResult<Response> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let c = st.cluster(&cluster).await?;
    let _ = c.get_any(&ns, &name).await?;
    let target = match q.kind.as_deref() {
        Some("stdout") => LogTarget::JobManagerStdout,
        _ => LogTarget::JobManagerMain,
    };
    match st.rest_for(&cluster, &ns, &name).await?.log(target).await {
        Ok(text) => {
            // JM 的日志文件可能有几十 MB：按行切尾部，默认 2000 行
            let tail = q.tail.unwrap_or(2000).clamp(1, 20_000) as usize;
            let out = if text.lines().count() > tail {
                let mut lines: Vec<&str> = text.lines().collect();
                let start = lines.len() - tail;
                lines.drain(..start);
                let mut s = lines.join("\n");
                s.push('\n');
                s
            } else {
                text
            };
            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                out,
            )
                .into_response())
        }
        Err(e) => Err(ApiError(Error::Upstream {
            target: format!("{name} 日志"),
            message: format!(
                "{e}；K8s 默认只输出 stdout，REST 文件日志需要 log4j 配 file appender，生产请用 Pod logs"
            ),
        })),
    }
}

// ---------------------------------------------------------------- 写操作

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodLogQuery {
    /// jm（默认）| tm
    pub kind: Option<String>,
    /// 只取最后 N 行（默认 2000）
    pub tail: Option<i64>,
}

/// GET /api/jobs/{c}/{ns}/{n}/pod-log —— 直接读 Pod 日志（K8s API）。
///
/// Flink 的 `/jobmanager/log` 依赖 log4j file appender，大多数集群拿不到；
/// 这时唯一可用的就是 Pod stdout。没有这个入口，"看日志"在产品里是死路。
pub async fn pod_log(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
    Query(q): Query<PodLogQuery>,
) -> ApiResult<Response> {
    let AuthUser { user, .. } = u;
    let JobTarget { cluster, ns, name } = j;

    let which = q.kind.unwrap_or_else(|| "jm".into());
    let (label, selector) = match which.as_str() {
        "tm" => ("taskmanager", format!("component=taskmanager,app={name}")),
        _ => ("jobmanager", format!("component=jobmanager,app={name}")),
    };
    let c = st.cluster(&cluster).await?;
    let (pod, text) = c.pod_logs(&ns, &selector, q.tail.unwrap_or(2000)).await?;
    st.audit(
        &user,
        "job.podlog",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("{which}:{pod}")),
        None,
    );
    let label = label.to_string();
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::HeaderName::from_static("x-flinkops-pod"), pod),
            (header::HeaderName::from_static("x-flinkops-role"), label),
        ],
        text,
    )
        .into_response())
}

pub async fn events(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let items = st
        .cluster(&cluster)
        .await?
        .list_events(&ns, crd::KIND_DEPLOYMENT, &name)
        .await?;
    Ok(Json(json!({ "items": items, "count": items.len() })))
}

pub async fn exceptions(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let (_kind, obj) = st.cluster(&cluster).await?.get_any(&ns, &name).await?;
    let job_id = crd::s(&obj, &["status", "jobStatus", "jobId"])
        .ok_or_else(|| Error::Conflict(format!("{name} 还没有 JobID（未提交或正在调谐）")))?;
    let v = st
        .rest_for(&cluster, &ns, &name)
        .await?
        .exceptions(&job_id)
        .await
        .map_err(|e| {
            ApiError(Error::Upstream {
                target: format!("{name} exceptions"),
                message: format!("{e}；可改用「打开 Flink UI」或 Pod 日志查看根因"),
            })
        })?;
    Ok(Json(v))
}
// ---------------------------------------------------------------- 背压 / 火焰图

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexQuery {
    /// 顶点 id；缺省取作业的第一个 vertex
    pub vertex: Option<String>,
    /// 火焰图类型：FULL | ON_CPU | OFF_CPU
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// 背压采样次数（Flink 默认 100，越小越快但噪声大）
    pub samples: Option<i64>,
}

async fn job_and_vertex(
    st: &Arc<AppState>,
    cluster: &str,
    ns: &str,
    name: &str,
    want: Option<&str>,
) -> Result<(String, String), ApiError> {
    let obj = st
        .cluster(cluster)
        .await?
        .get_raw(crd::KIND_DEPLOYMENT, ns, name)
        .await?;
    let job_id = crd::s(&obj, &["status", "jobStatus", "jobId"])
        .ok_or_else(|| ApiError(Error::Conflict(format!("{name} 还没有 JobID"))))?;

    let rest = st.rest_for(cluster, ns, name).await?;
    let vid = match want {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => {
            let job = rest.job(&job_id).await?;
            job.get("vertices")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| ApiError(Error::Conflict(format!("{name} 还没有 vertex"))))?
        }
    };
    Ok((job_id, vid))
}

/// GET /api/jobs/{c}/{ns}/{n}/backpressure?vertex=&samples=
///
/// 注意：Flink 每次请求会采样 100 次任务线程，**不要高频轮询**（30–60s 一次）。
pub async fn backpressure(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
    Query(q): Query<VertexQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let (job_id, vid) = job_and_vertex(&st, &cluster, &ns, &name, q.vertex.as_deref()).await?;
    // 背压采样会让 JM 连发 ~100 次请求：30s 内复用结果，避免轮询把 JM 打满
    let key = format!("bp/{cluster}/{ns}/{name}/{vid}");
    if let Some(mut cached) = st.heavy_get(&key, std::time::Duration::from_secs(30)).await {
        if let Some(obj) = cached.as_object_mut() {
            obj.insert("cached".into(), json!(true));
        }
        return Ok(Json(cached));
    }
    let rest = st.rest_for(&cluster, &ns, &name).await?;
    let mut v = rest.backpressure(&job_id, &vid).await?;
    if let (Some(n), Some(obj)) = (q.samples, v.as_object_mut()) {
        obj.insert("requestedSamples".into(), json!(n));
    }
    if let Some(obj) = v.as_object_mut() {
        obj.insert("vertex".into(), json!(vid));
    }
    st.heavy_put(&key, v.clone()).await;
    Ok(Json(v))
}

/// GET /api/jobs/{c}/{ns}/{n}/flamegraph?vertex=&type=FULL
///
/// Flink 返回的是 Traces JSON（不是 SVG）。这里原样透传，前端做摘要展示；
/// 完整可视化仍在 Flink UI 里看（用「打开 Flink UI」）。
pub async fn flamegraph(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
    Query(q): Query<VertexQuery>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let JobTarget { cluster, ns, name } = j;
    crate::mcp_guard::check_job_scope(&st, &headers, &ns, &name)
        .await
        .map_err(ApiError)?;
    let (job_id, vid) = job_and_vertex(&st, &cluster, &ns, &name, q.vertex.as_deref()).await?;
    let kind = q.kind.unwrap_or_else(|| "FULL".into());
    if !["FULL", "ON_CPU", "OFF_CPU"].contains(&kind.as_str()) {
        return Err(ApiError(Error::BadRequest(format!(
            "type 只能是 FULL / ON_CPU / OFF_CPU，收到 {kind}"
        ))));
    }
    // 火焰图同样会让 JM 采样，60s 内复用
    let key = format!("fg/{cluster}/{ns}/{name}/{vid}/{kind}");
    if let Some(v) = st.heavy_get(&key, std::time::Duration::from_secs(60)).await {
        return Ok(Json(v));
    }
    let rest = st.rest_for(&cluster, &ns, &name).await?;
    let v = rest.vertex_flamegraph(&job_id, &vid, &kind).await?;
    st.heavy_put(&key, v.clone()).await;
    Ok(Json(v))
}

/// POST /api/jobs/{c}/{ns}/{n}/profile —— 触发 async-profiler（FLIP-375，Flink 1.18+）
pub async fn profile(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    // async-profiler 会在 JM 上采样 30s，是重操作：readonly + 闸门都要过
    gate(
        &st,
        &headers,
        "flinkops_profile",
        &ns,
        &name,
        json!({}),
        true,
    )
    .await?;
    let (_kind, obj) = st.cluster(&cluster).await?.get_any(&ns, &name).await?;
    let _ = crd::s(&obj, &["status", "jobStatus", "jobId"])
        .ok_or_else(|| ApiError(Error::Conflict(format!("{name} 还没有 JobID"))))?;
    let rest = st.rest_for(&cluster, &ns, &name).await?;
    let v = rest.profiler_jobmanager(30).await.map_err(|e| {
        ApiError(Error::Upstream {
            target: format!("{name} profiler"),
            message: format!("{e}；async-profiler 需要 Flink 1.18+（FLIP-375）"),
        })
    })?;
    st.audit(
        &user,
        "job.profile",
        &name,
        &cluster,
        &ns,
        "ok",
        Some("jm profiler 30s".into()),
        None,
    );
    Ok(Json(v))
}
// ---------------------------------------------------------------- 删除 / 升级 / Ingress
