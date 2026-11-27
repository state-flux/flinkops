//! 作业变更运维：镜像、并行度、REST 暴露方式、autoscaler。
//!
//! 共同约束：只对 `FlinkDeployment` 生效；`FlinkSessionJob` 的同名能力属于它挂靠的
//! 会话集群，这里直接返回 400 并指出归属，避免"点下去 404"。

#![allow(unused_imports)]
use super::super::*;
use super::autoscaler;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageBody {
    pub image: String,
    /// 升级模式：savepoint（默认，保状态）| last-state | stateless
    pub upgrade_mode: Option<String>,
}

/// 升级模式是否依赖 savepoint 目录（stateless 不需要）。
fn needs_savepoint_dir(mode: &str) -> bool {
    mode != "stateless"
}

/// 从 CRD 对象里取 state.savepoints.dir（空串视同未配置）。
fn savepoint_dir(obj: &Value) -> String {
    flinkops_types::crd::s(obj, &["spec", "flinkConfiguration", "state.savepoints.dir"])
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub async fn set_image(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<ImageBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;
    if body.image.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("image 不能为空".into())));
    }
    let mode = body.upgrade_mode.unwrap_or_else(|| "savepoint".into());
    if !["savepoint", "last-state", "stateless"].contains(&mode.as_str()) {
        return Err(ApiError(Error::BadRequest(format!(
            "upgrade_mode 只能是 savepoint / last-state / stateless，收到 {mode}"
        ))));
    }
    gate(
        &st,
        &headers,
        "flinkops_upgrade_image",
        &ns,
        &name,
        json!({ "image": body.image, "upgradeMode": mode }),
        true,
    )
    .await?;

    let c = st.cluster(&cluster).await?;
    let (kind, obj) = c.get_any(&ns, &name).await?;
    if kind != crd::KIND_DEPLOYMENT {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 {kind}：镜像属于它挂靠的会话集群，请到「会话集群」里升级"
        ))));
    }
    // 前置校验：savepoint / last-state 升级依赖 savepoint 目录。
    // 没配置时 operator 会先触发 savepoint，失败后把作业留在原地——
    // 用户看到的现象是"点了升级，转一圈没反应"。
    if needs_savepoint_dir(&mode) && savepoint_dir(&obj).is_empty() {
        return Err(ApiError(Error::BadRequest(format!(
            "upgradeMode={mode} 需要先配置 state.savepoints.dir（作业 flinkConfiguration 或环境基线），否则升级会卡在 savepoint 失败；确认不需要保状态请改用 upgradeMode=stateless"
        ))));
    }
    let patch = json!({
        "spec": {
            "image": body.image.trim(),
            "job": { "upgradeMode": mode }
        }
    });
    c.patch_merge(&kind, &ns, &name, patch).await?;
    st.audit(
        &user,
        "job.upgrade",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("image={} upgradeMode={mode}", body.image.trim())),
        None,
    );
    Ok(Json(
        json!({ "ok": true, "image": body.image.trim(), "upgradeMode": mode }),
    ))
}

pub async fn set_autoscaler(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<AutoscalerBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;
    let (cfg, ignored) =
        autoscaler::build_config(&body).map_err(|e| ApiError(Error::BadRequest(e)))?;
    gate(
        &st,
        &headers,
        "flinkops_autoscaler",
        &ns,
        &name,
        json!({ "enabled": body.enabled, "keys": cfg.keys().collect::<Vec<_>>() }),
        true,
    )
    .await?;

    let c = st.cluster(&cluster).await?;
    // autoscaler 是 FlinkDeployment 的 flinkConfiguration；SessionJob 没有这一层
    let (kind, _obj) = c.get_any(&ns, &name).await?;
    if kind != crd::KIND_DEPLOYMENT {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 {kind}：autoscaler 配置属于 FlinkDeployment，请到它挂靠的会话集群上设置"
        ))));
    }
    c.patch_merge(
        &kind,
        &ns,
        &name,
        json!({ "spec": { "flinkConfiguration": cfg } }),
    )
    .await?;
    st.audit(
        &user,
        "job.autoscaler",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("enabled={}", body.enabled)),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "enabled": body.enabled,
        // operator 不认识的输入要明说，不能静默丢弃
        "ignored": ignored,
    })))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleBody {
    pub parallelism: Option<i64>,
    pub task_manager_replicas: Option<i64>,
}

pub async fn scale(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<ScaleBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;
    gate(
        &st,
        &headers,
        "flinkops_scale",
        &ns,
        &name,
        json!({ "parallelism": body.parallelism, "taskManagerReplicas": body.task_manager_replicas }),
        true,
    )
    .await?;
    if body.parallelism.is_none() && body.task_manager_replicas.is_none() {
        return Err(ApiError(Error::BadRequest(
            "parallelism 与 task_manager_replicas 至少填一个".into(),
        )));
    }
    let mut spec = json!({});
    if let Some(p) = body.parallelism {
        if p <= 0 {
            return Err(ApiError(Error::BadRequest("parallelism 必须大于 0".into())));
        }
        spec["job"] = json!({ "parallelism": p });
    }
    if let Some(r) = body.task_manager_replicas {
        if r <= 0 {
            return Err(ApiError(Error::BadRequest(
                "task_manager_replicas 必须大于 0".into(),
            )));
        }
        spec["taskManager"] = json!({ "replicas": r });
    }
    let c = st.cluster(&cluster).await?;
    // SessionJob 没有自己的 taskManager：副本属于它挂靠的 session 集群，
    // 这里改不了（早期硬编码 KIND_DEPLOYMENT，对 SessionJob 直接 404）。
    let (kind, obj) = c.get_any(&ns, &name).await?;
    if kind == crd::KIND_SESSION_JOB && body.task_manager_replicas.is_some() {
        let owner = flinkops_types::crd::s(&obj, &["spec", "deploymentName"]).unwrap_or_default();
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 FlinkSessionJob，taskManagerReplicas 属于它挂靠的会话集群 {owner}，请到「会话集群」里改"
        ))));
    }
    // 槽位总量校验：parallelism 超过 TM 副本 × 每 TM 槽位数时，operator 会一直
    // 卡在 "not enough slots"，用户看到的现象是"扩容没生效"。这里提前拦。
    if let Some(p) = body.parallelism {
        let slots_per_tm = flinkops_types::crd::s(
            &obj,
            &[
                "spec",
                "flinkConfiguration",
                "taskmanager.numberOfTaskSlots",
            ],
        )
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
        let tm_now = flinkops_types::crd::s(&obj, &["spec", "taskManager", "replicas"])
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1)
            .max(1);
        let tm_next = body.task_manager_replicas.unwrap_or(tm_now).max(1);
        let total = slots_per_tm * tm_next;
        if p > total {
            let need = (p + slots_per_tm - 1) / slots_per_tm;
            return Err(ApiError(Error::BadRequest(format!(
                "并行度 {p} 超过槽位总量 {total}（{tm_next} 个 TM × {slots_per_tm} 槽），需同时把 TM 提到 {need}"
            ))));
        }
    }

    c.patch_merge(&kind, &ns, &name, json!({ "spec": spec }))
        .await?;
    st.audit(
        &user,
        "job.scale",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!(
            "parallelism={:?} tm={:?}",
            body.parallelism, body.task_manager_replicas
        )),
        None,
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestExposedBody {
    /// ClusterIP | NodePort | LoadBalancer
    pub kind: String,
}

pub async fn set_rest_exposed(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<RestExposedBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;
    let kind = body.kind.trim().to_string();
    if !["ClusterIP", "NodePort", "LoadBalancer"].contains(&kind.as_str()) {
        return Err(ApiError(Error::BadRequest(
            "kind 只能是 ClusterIP / NodePort / LoadBalancer".into(),
        )));
    }
    let c = st.cluster(&cluster).await?;
    let (obj_kind, _obj) = c.get_any(&ns, &name).await?;
    if obj_kind != crd::KIND_DEPLOYMENT {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 {obj_kind}：REST 暴露方式属于 FlinkDeployment，请到它挂靠的会话集群上改"
        ))));
    }
    let patch = json!({
        "spec": { "flinkConfiguration": { "kubernetes.rest-service.exposed.type": kind } }
    });
    c.patch_merge(&obj_kind, &ns, &name, patch).await?;
    st.audit(
        &user,
        "job.rest-exposed",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("type={kind}（会重建 Service，JM 重启后生效）")),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "type": kind,
        "note": "已写入 flinkConfiguration；operator 重建 Service 后生效（约 30-60s）",
    })))
}

#[cfg(test)]
mod preflight_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stateless_never_needs_savepoint_dir() {
        assert!(!needs_savepoint_dir("stateless"));
    }

    #[test]
    fn savepoint_and_last_state_need_dir() {
        assert!(needs_savepoint_dir("savepoint"));
        assert!(needs_savepoint_dir("last-state"));
    }

    #[test]
    fn savepoint_dir_read_from_flink_configuration() {
        let obj = json!({
            "spec": { "flinkConfiguration": { "state.savepoints.dir": "s3://bucket/sp" } }
        });
        assert_eq!(savepoint_dir(&obj), "s3://bucket/sp");
    }

    #[test]
    fn missing_or_blank_dir_is_empty() {
        assert_eq!(savepoint_dir(&json!({ "spec": {} })), "");
        assert_eq!(
            savepoint_dir(&json!({
                "spec": { "flinkConfiguration": { "state.savepoints.dir": "   " } }
            })),
            ""
        );
    }
}
