//! 作业生命周期：状态切换 / 重启 / savepoint / 取消 / 回滚 / 从 savepoint 恢复 / 删除。

// 子模块通过父模块拿到全部共享 import 与 helper（child 可见父的私有项）
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
pub struct StateRequest {
    pub running: bool,
}

pub async fn set_state(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(req): Json<StateRequest>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        if req.running {
            "flinkops_resume"
        } else {
            "flinkops_suspend"
        },
        &ns,
        &name,
        json!({ "running": req.running }),
        !req.running, // 挂起要审批；启动不需要
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    // 启动前先清残留：suspend 会留下 `<name>-rest` Service，JM Deployment 已删；
    // operator 随后报 `The Flink cluster <name> already exists` 并重试到天荒地老
    // （线上实测过：重启卡 6 分钟，operator 一直重试）。残留清掉再起。
    let (kind, obj) = c.get_any(&ns, &name).await?;
    let target_state = if req.running { "running" } else { "suspended" };
    // 已经在目标状态：不重复 patch（避免无意义的 generation+1 与一轮调谐）
    if crd::s(&obj, &["spec", "job", "state"])
        .map(|s| s.eq_ignore_ascii_case(target_state))
        .unwrap_or(false)
    {
        return Ok(Json(json!({
            "ok": true,
            "state": target_state,
            "clearedStale": Vec::<String>::new(),
            "note": "已经是目标状态，未做任何改动",
        })));
    }
    // 用读到的 resourceVersion 做 CAS：与并发改动（另一个窗口/另一个实例）冲突时
    // 返回 409，而不是"后写覆盖前写"。
    let rv = crd::s(&obj, &["metadata", "resourceVersion"]).unwrap_or_default();
    let cleared = if req.running {
        c.clear_stale_before_start(&ns, &name)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let target_state = if req.running { "running" } else { "suspended" };
    c.patch_merge_cas(
        &kind,
        &ns,
        &name,
        &rv,
        json!({ "spec": { "job": { "state": target_state } } }),
    )
    .await?;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        if req.running { "job.start" } else { "job.stop" },
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!(
            "state→{target_state}{}",
            if cleared.is_empty() {
                String::new()
            } else {
                format!("；清理残留：{}", cleared.join(","))
            }
        )),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "state": if req.running { "running" } else { "suspended" },
        "clearedStale": cleared
    })))
}

pub async fn restart(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        "flinkops_restart",
        &ns,
        &name,
        json!({}),
        false,
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    let (kind, obj) = c.get_any(&ns, &name).await?;
    // 挂起态 bump nonce 什么也不会发生；返回 200 只会让运维以为"重启了"。
    if crd::s(&obj, &["spec", "job", "state"])
        .map(|s| s.eq_ignore_ascii_case("suspended"))
        .unwrap_or(false)
    {
        return Err(ApiError(Error::Conflict(format!(
            "{name} 处于挂起状态，重启没有意义；请先「启动」"
        ))));
    }
    let nonce = c.bump_restart_nonce_kind(&kind, &ns, &name).await?;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        "job.restart",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("restartNonce={nonce}")),
        None,
    );
    Ok(Json(json!({ "ok": true, "restartNonce": nonce })))
}

#[derive(Deserialize)]
pub struct SavepointRequest {
    #[serde(default)]
    pub path: Option<String>,
}

pub async fn savepoint(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    // body 必填（可以是 `{}`）：用 Option<Json<T>> 时，缺 Content-Type 的请求
    // 会被 axum 直接判成 None，用户传的 path 静默丢失却仍返回成功。
    Json(body): Json<SavepointRequest>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    let path = body.path;
    gate(
        &st,
        &headers,
        "flinkops_savepoint",
        &ns,
        &name,
        json!({ "path": path }),
        false, // savepoint 不改变运行状态，不需要审批
    )
    .await?;
    let c = st.cluster(&cluster).await?;

    // 前置状态守卫：对已挂起/无 JobID 的作业打快照，CR 会被 operator 判成
    // ABANDONED（"secondary resource is not running"），接口却返回 ok。
    let (kind, obj) = c.get_any(&ns, &name).await?;
    if kind != crd::KIND_DEPLOYMENT {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 {kind}；SessionJob 没有自己的 savepoint，请到所属 session 集群（Flink UI / kubectl）操作"
        ))));
    }
    if crd::s(&obj, &["spec", "job", "state"]).as_deref() == Some("suspended") {
        return Err(ApiError(Error::Conflict(format!(
            "{name} 处于挂起状态（spec.job.state=suspended），先启动再打 savepoint"
        ))));
    }
    if crd::s(&obj, &["status", "jobStatus", "jobId"]).is_none() {
        return Err(ApiError(Error::Conflict(format!(
            "{name} 还没有运行中的 JobID，无法触发 savepoint"
        ))));
    }
    // 作业在重启/调谐中时打快照，operator 只会把它判成 ABANDONED。
    // 与其等几秒后报错，不如直接说清楚现在什么状态。
    if let Some(js) = crd::s(&obj, &["status", "jobStatus", "state"]) {
        if !js.eq_ignore_ascii_case("RUNNING") {
            return Err(ApiError(Error::Conflict(format!(
                "{name} 当前 jobStatus.state={js}（不是 RUNNING），等作业稳定后再打 savepoint"
            ))));
        }
    }
    let path = match path {
        Some(p) => Some(normalize_savepoint_path(&p)?),
        None => None,
    };

    // 优先建 FlinkStateSnapshot（可追溯）；若 CR 被 webhook 拒绝，降级为直接打 JM REST。
    // 降级必须在响应与审计里显式标注：CR 不存在 → 平台"快照"页看不到这一条，
    // 恢复入口也选不到它，运维不能以为拿到了正式快照。
    match c.create_savepoint(&ns, &name, path.as_deref()).await {
        Ok(snapshot) => {
            // FlinkStateSnapshot 是异步的：短暂等一下读 status.state，
            // 只为抓住"立刻失败"的情况（作业刚重启 → ABANDONED）。
            // 正常执行通常要几十秒，不能在这里同步等，否则每次触发都卡 6s。
            let (state, err) = wait_snapshot_state(&c, &ns, &snapshot, 2_000).await;
            if matches!(state.as_str(), "FAILED" | "ABANDONED") {
                st.audit(
                    &user,
                    "savepoint.trigger",
                    &name,
                    &cluster,
                    &ns,
                    "error",
                    Some(format!(
                        "cr:{snapshot} state={state} err={}",
                        err.as_deref().unwrap_or("-")
                    )),
                    None,
                );
                return Err(ApiError(Error::Upstream {
                    target: format!("{ns}/{name} savepoint {snapshot}"),
                    message: format!(
                        "快照 CR 已创建但执行失败（state={state}）：{}",
                        err.unwrap_or_else(|| "operator 未给出原因，见快照详情".into())
                    ),
                }));
            }
            st.audit(
                &user,
                "savepoint.trigger",
                &name,
                &cluster,
                &ns,
                "ok",
                Some(format!("cr:{snapshot} state={state}")),
                None,
            );
            Ok(Json(json!({
                "ok": true,
                "mode": "cr",
                "snapshot": snapshot,
                "state": state,
                "degraded": false,
            })))
        }
        Err(e @ (Error::Forbidden(_) | Error::NotFound(_))) => {
            let msg = e.message();
            let obj = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;
            let job_id = crd::s(&obj, &["status", "jobStatus", "jobId"])
                .ok_or_else(|| ApiError(Error::Conflict(format!("{name} 还没有 JobID"))))?;
            let rest = st.rest_for(&cluster, &ns, &name).await?;
            let resp = rest.trigger_savepoint(&job_id, path.as_deref()).await?;
            let trigger = resp
                .get("request-id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            st.audit(
                &user,
                "savepoint.trigger",
                &name,
                &cluster,
                &ns,
                "ok",
                Some(format!(
                    "degraded:rest:{trigger}（CR 被 webhook 拒绝：{msg}）"
                )),
                None,
            );
            Ok(Json(json!({
                "ok": true,
                "mode": "rest",
                "snapshot": Value::Null,
                "degraded": true,
                "requestId": trigger,
                "note": "FlinkStateSnapshot CR 被 webhook 拒绝，已改用 JobManager REST 触发；该快照不会出现在平台的快照列表/恢复下拉里",
            })))
        }
        Err(e) => Err(ApiError(e)),
    }
}
// ---------------------------------------------------------------- 提交作业

/// DELETE /api/jobs/{c}/{ns}/{n} —— 删除 FlinkDeployment（不可逆）
pub async fn delete_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        "flinkops_delete_job",
        &ns,
        &name,
        json!({ "name": name }),
        true,
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    let (kind, _) = c.get_any(&ns, &name).await?;
    c.delete_object(&kind, &ns, &name).await?;
    // 删完立刻清索引：否则列表里会留一条点进去 404 的"幽灵行"
    let _ = st.store.delete_job(&cluster, &ns, &name).await;
    st.audit(&user, "job.delete", &name, &cluster, &ns, "ok", None, None);
    Ok(Json(json!({ "ok": true, "deleted": name })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RestoreBody {
    /// 从哪个 savepoint 恢复；留空用 operator 记录的最近一次
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RollbackBody {
    /// 目标镜像；留空则从审计历史里找"上一个不同的镜像"
    #[serde(default)]
    pub image: Option<String>,
}

/// POST /api/jobs/{c}/{ns}/{n}/cancel —— 立即取消（**不保留状态**）
///
/// 与"挂起"的区别：`upgradeMode=savepoint` 的作业挂起时会先打一份 savepoint
/// （占 HDFS、慢几十秒），而 cancel 的语义是"这个作业我不要了/先止损"，
/// 所以先把 `upgradeMode` 切成 stateless 再 suspend，确保不产生新快照。
pub async fn cancel_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        "flinkops_suspend",
        &ns,
        &name,
        json!({ "cancel": true }),
        true,
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    let (kind, obj) = c.get_any(&ns, &name).await?;
    let rv = crd::s(&obj, &["metadata", "resourceVersion"]).unwrap_or_default();
    let before = crd::s(&obj, &["spec", "job", "upgradeMode"]).unwrap_or_default();
    c.patch_merge_cas(
        &kind,
        &ns,
        &name,
        &rv,
        json!({ "spec": { "job": { "state": "suspended", "upgradeMode": "stateless" } } }),
    )
    .await?;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        "job.cancel",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!(
            "cancel: state→suspended, upgradeMode {before}→stateless（不保留状态）"
        )),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "state": "suspended",
        "upgradeMode": "stateless",
        "previousUpgradeMode": before,
        "note": "已取消：不会产生新的 savepoint；要保留状态请改用「挂起」",
    })))
}

/// POST /api/jobs/{c}/{ns}/{n}/rollback —— 回滚镜像
///
/// 不传 `image` 时从审计里的 `job.upgrade` 记录找"上一个不同的镜像"，
/// 这样"刚升级完发现不对"可以一键回退，而不是让运维去翻聊天记录。
pub async fn rollback(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<RollbackBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        "flinkops_upgrade_image",
        &ns,
        &name,
        json!({ "rollback": body.image }),
        true,
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    let obj = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;
    let cur_image = crd::s(&obj, &["spec", "image"]).unwrap_or_default();
    let cur_mode = crd::s(&obj, &["spec", "job", "upgradeMode"]).unwrap_or_default();
    if cur_mode == "stateless" {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 的 upgradeMode=stateless，回滚镜像不会保留状态；如确认要继续，请显式传 image 并改用 savepoint 模式"
        ))));
    }

    let target = match body
        .image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(img) => img.to_string(),
        None => {
            let entries = st.store.list_audit(500).await?;
            entries
                .iter()
                .filter(|e| {
                    e.action == "job.upgrade"
                        && e.target == name
                        && e.cluster == cluster
                        && e.namespace == ns
                })
                .filter_map(|e| e.detail.as_deref())
                .filter_map(|d| {
                    d.split_whitespace()
                        .find_map(|kv| kv.strip_prefix("image="))
                        .map(str::to_string)
                })
                .find(|img| *img != cur_image)
                .ok_or_else(|| {
                    ApiError(Error::Conflict(format!(
                        "{name} 没有可回滚的历史镜像（审计里没找到 job.upgrade），请显式传 image"
                    )))
                })?
        }
    };
    if target == cur_image {
        return Err(ApiError(Error::Conflict(format!(
            "{name} 已经是 {cur_image}，无需回滚"
        ))));
    }

    patch_cas(
        &c,
        crd::KIND_DEPLOYMENT,
        &ns,
        &name,
        json!({ "spec": { "image": target.clone(), "job": { "upgradeMode": "savepoint" } } }),
    )
    .await?;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        "job.rollback",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!(
            "from={cur_image} to={target} upgradeMode=savepoint"
        )),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "from": cur_image,
        "to": target,
        "upgradeMode": "savepoint",
        "note": "已回滚镜像；operator 会按 savepoint 升级流程重建作业",
    })))
}

/// POST /api/jobs/{c}/{ns}/{n}/restore —— 从 savepoint 恢复
///
/// 规则（都是踩过的坑）：
/// - 不改写 `upgradeMode`：`stateless` 作业从 savepoint 恢复等于"有状态却不读状态"，
///   必须由用户显式改模式，而不是平台替他改；
/// - `path` 必须是绝对 URI 目录（`hdfs://.../sp-1/`），否则 operator 运行期才报错；
/// - 不传 path 时用 operator 记录的最近一次 savepoint，而不是"现场再打一个"。
pub async fn restore(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    // body 必填（可为 `{}`）：Option<Json<T>> 会在缺 Content-Type 时静默吞掉 path
    Json(body): Json<RestoreBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    let path_in = body.path;
    gate(
        &st,
        &headers,
        "flinkops_restore",
        &ns,
        &name,
        json!({ "savepointPath": path_in }),
        true,
    )
    .await?;

    let c = st.cluster(&cluster).await?;
    let obj = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;
    let mode = crd::s(&obj, &["spec", "job", "upgradeMode"]).unwrap_or_default();
    let last_sp = crd::s(
        &obj,
        &[
            "status",
            "jobStatus",
            "savepointInfo",
            "lastSavepoint",
            "location",
        ],
    );

    let path = match path_in {
        Some(p) => p.trim().to_string(),
        None => last_sp.ok_or_else(|| {
            ApiError(Error::Conflict(format!(
                "{name} 没有记录到可用的 savepoint，请显式传 path"
            )))
        })?,
    };
    // Flink/operator 记录的 savepoint 目录天然不带尾斜杠（`…/savepoint-abc-123`），
    // 所以这里**归一化**而不是拒绝：补一个 `/` 再交给 operator。
    let path = normalize_savepoint_path(&path)?;

    if mode == "stateless" {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 的 upgradeMode=stateless，从 savepoint 恢复不会加载状态。\
             请先用 POST /api/jobs/{cluster}/{ns}/{name}/image 把 upgradeMode 改成 savepoint 或 last-state"
        ))));
    }

    // 对已调谐的作业，operator 只在"重新部署"时才读 `initialSavepointPath`；
    // 直接 patch 一个 running 的作业 = 什么都不发生（旧实现就是这种"假成功"）。
    // 正确顺序：先挂起 → 写路径 → 再启动，并等到 jobId 变化才算恢复成功。
    let job_id_before = crd::s(&obj, &["status", "jobStatus", "jobId"]);
    let already_suspended = crd::s(&obj, &["spec", "job", "state"])
        .map(|s| s.eq_ignore_ascii_case("suspended"))
        .unwrap_or(false);
    let mut phases: Vec<String> = Vec::new();

    if !already_suspended {
        patch_cas(
            &c,
            crd::KIND_DEPLOYMENT,
            &ns,
            &name,
            json!({ "spec": { "job": { "state": "suspended" } } }),
        )
        .await?;
        phases.push("suspend".into());
        let ok = wait_lifecycle(&c, &ns, &name, "SUSPENDED", 90).await;
        if !ok {
            refresh_index(&st, &cluster, &ns, &name).await;
            return Err(ApiError(Error::Conflict(format!(
                "{name} 在 90s 内没有进入 SUSPENDED，恢复未执行（可能卡在调谐/升级中）。已完成阶段：{}",
                phases.join(" → ")
            ))));
        }
    }

    patch_cas(
        &c,
        crd::KIND_DEPLOYMENT,
        &ns,
        &name,
        json!({ "spec": { "job": { "initialSavepointPath": path, "state": "running" } } }),
    )
    .await?;
    phases.push("start".into());

    // 等作业真的起来：jobId 变化才算"从指定 savepoint 重建"。
    let confirmed = wait_job_restarted(&c, &ns, &name, job_id_before.as_deref(), 180).await;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        "job.restore",
        &name,
        &cluster,
        &ns,
        if confirmed { "ok" } else { "error" },
        Some(format!(
            "from={path} upgradeMode={mode} phases={} confirmed={confirmed}",
            phases.join(" → ")
        )),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "from": path,
        "upgradeMode": mode,
        "confirmed": confirmed,
        "phases": phases,
        "note": if confirmed {
            "已从指定 savepoint 重建，并确认作业已运行"
        } else {
            "已提交恢复（挂起 → 写路径 → 启动），但 180s 内没观察到新 jobId，请到详情页确认状态"
        },
    })))
}
