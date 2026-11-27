//! 会话集群（FlinkDeployment.mode=session）：列表。
//!
//! 后续可加：创建 / 启停 / 空闲 TTL 回收（见 docs/api-spec.md 的 roadmap）。
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::Json;
use flinkops_core as core;
use flinkops_types::{crd, Error, Severity};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::{AuthUser, RequireWrite};

use crate::state::AppState;

pub async fn list(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { headers, .. } = u;
    let only = q.get("cluster").cloned();
    let mut out = Vec::new();
    let tok = crate::mcp_guard::peek(&st, &headers).await?;

    for (id, c) in &st.cluster_entries().await {
        if let Some(want) = &only {
            if want != id {
                continue;
            }
        }
        for obj in c.list_sessions(None).await? {
            let s = crd::summary(id, crd::KIND_DEPLOYMENT, &obj);
            if let Some(t) = &tok {
                if !crate::mcp_guard::job_allowed(t, &s.namespace, &s.name) {
                    continue;
                }
            }
            out.push(json!({
                "cluster": id,
                "namespace": s.namespace,
                "name": s.name,
                "state": s.state,
                "flinkVersion": s.flink_version,
                "image": s.image,
                "jobManagerCpu": crd::s(&obj, &["spec", "jobManager", "resource", "cpu"]),
                "jobManagerMemory": crd::s(&obj, &["spec", "jobManager", "resource", "memory"]),
                "taskManagerReplicas": crd::i(&obj, &["spec", "taskManager", "replicas"]).unwrap_or(0),
                "slotsPerTm": s.slots_per_tm.unwrap_or(0),
                "usedSlots": Value::Null,
                "idleTtl": crd::s(&obj, &["spec", "flinkConfiguration", "kubernetes.operator.session.idle.ttl"]),
                "sqlGateway": Value::Null,
            }));
        }
    }
    Ok(Json(json!({ "items": out })))
}
// ---------------------------------------------------------------- 创建会话集群

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub name: String,
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    pub profile_id: Option<String>,
    pub slots: Option<i64>,
    pub task_manager_replicas: Option<i64>,
    pub job_manager_memory: Option<String>,
    /// 覆盖基线里的 Flink 版本（如 v1_18）
    pub flink_version: Option<String>,
    /// 覆盖基线里的镜像
    pub image: Option<String>,
    pub task_manager_memory: Option<String>,
    #[serde(default, rename = "dryRun")]
    pub dry_run: bool,
}

/// 作业摘要里的版本是 `1.15`，CRD 的枚举要求 `v1_15`。
fn normalize_version(v: &str) -> String {
    let v = v.trim();
    if v.starts_with("v1_") || v.starts_with("v2_") {
        return v.to_string();
    }
    format!("v{}", v.trim_start_matches('v').replace('.', "_"))
}

/// POST /api/sessions —— 创建一个 session 模式的 FlinkDeployment。
pub async fn create(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    let cluster_id = match body.cluster.clone() {
        Some(c) => c,
        None if st.clusters_count().await == 1 => st
            .cluster_ids()
            .await
            .into_iter()
            .next()
            .unwrap_or_default(),
        None => {
            return Err(ApiError(Error::BadRequest(format!(
                "已接入 {} 个集群，请指定 cluster",
                st.clusters_count().await
            ))))
        }
    };

    let profiles = st.store.list_profiles().await?;
    let profile = match body.profile_id.as_deref() {
        Some(id) => profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| ApiError(Error::NotFound(format!("profile {id}"))))?,
        None => profiles
            .into_iter()
            .next()
            .ok_or_else(|| ApiError(Error::NotFound("没有任何环境基线".into())))?,
    };

    let ns = body
        .namespace
        .clone()
        .unwrap_or_else(|| profile.namespace.clone());
    crate::mcp_guard::gate(
        &st,
        &headers,
        "flinkops_create_session",
        &ns,
        body.name.trim(),
        serde_json::json!({ "slots": body.slots, "tm": body.task_manager_replicas }),
        false,
    )
    .await
    .map_err(ApiError)?;
    let slots = body.slots.unwrap_or(4).max(1);
    let tm = body.task_manager_replicas.unwrap_or(2).max(1);

    let overrides = json!({
        "metadata": { "name": body.name, "namespace": ns },
        // 新版本 operator 的 KubernetesDeploymentMode 只有 standalone / native。
        // session cluster 的正确表达是：mode=native + 不写 job 段（不是 mode=session）。
        "mode": "native",
        "flinkConfiguration": {
            "taskmanager.numberOfTaskSlots": slots.to_string(),
        },
        "taskManager": {
            "replicas": tm,
            "resource": {
                "cpu": 1,
                "memory": body.task_manager_memory.clone().unwrap_or_else(|| "4Gi".into()),
            }
        },
        "jobManager": {
            "replicas": 1,
            "resource": {
                "cpu": 1,
                "memory": body.job_manager_memory.clone().unwrap_or_else(|| "2Gi".into()),
            }
        }
    });

    let c = st.cluster(&cluster_id).await?;
    let mut spec = core::spec_from_profile(&profile, &overrides);
    // serviceAccount 必须真实存在，否则 Pod 起不来（基线里的 flink-sa 未必存在）
    let wanted = spec
        .get("serviceAccount")
        .and_then(|v| v.as_str())
        .unwrap_or("flink")
        .to_string();
    let cas = c.list_service_accounts(&ns).await.unwrap_or_default();
    let chosen_sa = if cas.iter().any(|s| s == &wanted) {
        wanted.clone()
    } else if cas.iter().any(|s| s == "flink") {
        "flink".to_string()
    } else if let Some(first) = cas.first() {
        first.clone()
    } else {
        return Err(ApiError(Error::BadRequest(format!(
            "命名空间 {ns} 里没有任何 ServiceAccount，无法创建 Pod；请先建一个（通常叫 flink）"
        ))));
    };

    // 镜像与版本必须跟真实集群对齐：基线里可能是外网镜像/别的版本，拉不到就 ImagePullBackOff
    let siblings = st
        .store
        .list_jobs(Some(&cluster_id))
        .await
        .unwrap_or_default();
    let top_of = |pick: fn(&flinkops_types::JobSummary) -> Option<String>| -> Option<String> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for j in siblings.iter().filter(|j| j.namespace == ns) {
            if let Some(v) = pick(j) {
                *counts.entry(v).or_default() += 1;
            }
        }
        counts.into_iter().max_by_key(|(_, c)| *c).map(|(v, _)| v)
    };
    let real_image = top_of(|j| j.image.clone());
    let real_version = top_of(|j| j.flink_version.clone());

    let mut applied_image = None;
    let mut applied_version = None;

    // session cluster 不跑具体作业：去掉 job 段（这是判定 session 的唯一依据）
    if let Some(obj) = spec.as_object_mut() {
        obj.remove("job");
        obj.insert("serviceAccount".into(), json!(chosen_sa));
        // 用户显式指定 → 用指定值；否则用集群里真实作业的镜像/版本兜底
        match body
            .image
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(img) => {
                obj.insert("image".into(), json!(img));
                applied_image = Some(img.to_string());
            }
            None => {
                if let Some(img) = &real_image {
                    obj.insert("image".into(), json!(img));
                    applied_image = Some(img.clone());
                }
            }
        }
        match body
            .flink_version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(v) => {
                let nv = normalize_version(v);
                obj.insert("flinkVersion".into(), json!(nv));
                applied_version = Some(nv);
            }
            None => {
                if let Some(v) = &real_version {
                    let nv = normalize_version(v);
                    obj.insert("flinkVersion".into(), json!(nv));
                    applied_version = Some(nv);
                }
            }
        }

        if let Some(img) = &body.image {
            obj.insert("image".into(), json!(img));
        }
    }

    let env = core::Env {
        in_cluster: st
            .cluster(&cluster_id)
            .await
            .map(|c| c.in_cluster)
            .unwrap_or(true),
    };
    let findings = core::validate_with_env(&spec, env);
    if core::has_errors(&findings) {
        let codes: Vec<String> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| format!("{}@{}", f.code, f.path))
            .collect();
        return Err(ApiError(Error::BadRequest(format!(
            "校验未通过：{}",
            codes.join(", ")
        ))));
    }

    let c = st.cluster(&cluster_id).await?;

    // 先校验命名空间：不存在时直接给出可用列表，而不是让 apply 抛 k8s 原始错误
    if !c.namespace_exists(&ns).await? {
        let known = st
            .store
            .list_jobs(Some(&cluster_id))
            .await
            .unwrap_or_default();
        let mut avail: Vec<String> = known.iter().map(|j| j.namespace.clone()).collect();
        avail.sort();
        avail.dedup();
        return Err(ApiError(Error::BadRequest(format!(
            "命名空间 {ns} 不存在。该集群当前有作业的命名空间：{}",
            if avail.is_empty() {
                "（暂无）".to_string()
            } else {
                avail.join(", ")
            }
        ))));
    }

    if c.get_raw(crd::KIND_DEPLOYMENT, &ns, &body.name)
        .await
        .is_ok()
    {
        return Err(ApiError(Error::Conflict(format!(
            "{ns}/{} 已存在",
            body.name
        ))));
    }

    let obj = json!({
        "apiVersion": format!("{}/{}", crd::GROUP, crd::VERSION),
        "kind": crd::KIND_DEPLOYMENT,
        "metadata": { "name": body.name, "namespace": ns },
        "spec": spec,
    });

    if !body.dry_run {
        c.create_object(crd::KIND_DEPLOYMENT, &ns, obj.clone())
            .await?;
    }

    st.audit(
        &user,
        "session.create",
        &body.name,
        &cluster_id,
        &ns,
        "ok",
        Some(format!(
            "slots={slots} tm={tm}{}",
            if body.dry_run { " (dryRun)" } else { "" }
        )),
        None,
    );

    Ok(Json(json!({
        "ok": true,
        "cluster": cluster_id,
        "namespace": ns,
        "name": body.name,
        "serviceAccount": chosen_sa,
        "image": applied_image,
        "flinkVersion": applied_version,
        "dryRun": body.dry_run,
    })))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlGatewayBody {
    /// 平台在集群外时需要 NodePort；默认 true
    pub node_port: Option<bool>,
    /// 指定端口（可选，不填由 k8s 分配）
    pub port: Option<i32>,
    /// 覆盖 gateway 镜像（默认沿用 session cluster 的镜像）
    pub image: Option<String>,
}

/// POST /api/sessions/{c}/{ns}/{n}/sql-gateway —— 给会话集群配一个 SQL Gateway
pub async fn deploy_sql_gateway(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Path((cluster, ns, name)): Path<(String, String, String)>,
    body: Option<Json<SqlGatewayBody>>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    let b = body.map(|j| j.0).unwrap_or(SqlGatewayBody {
        node_port: Some(true),
        port: None,
        image: None,
    });
    let want_np = b.node_port.unwrap_or(true);
    crate::mcp_guard::gate(
        &st,
        &headers,
        "flinkops_sql_gateway",
        &ns,
        &name,
        serde_json::json!({ "nodePort": want_np }),
        false,
    )
    .await
    .map_err(ApiError)?;

    let c = st.cluster(&cluster).await?;
    let obj = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;

    // session cluster 必须没有 job 段（否则它是 application 模式，gateway 连不上）
    if obj.get("spec").and_then(|s| s.get("job")).is_some() {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 application 模式（有 job 段）；SQL Gateway 需要一个 session cluster"
        ))));
    }

    let image = b
        .image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| crd::s(&obj, &["spec", "image"]))
        .ok_or_else(|| {
            ApiError(Error::BadRequest(
                "该集群没有 image，无法推断 SQL Gateway 镜像".into(),
            ))
        })?;
    let sa = crd::s(&obj, &["spec", "serviceAccount"]).unwrap_or_else(|| "flink".into());

    let (gw_name, port) = c
        .apply_sql_gateway(
            &ns,
            &name,
            &image,
            &sa,
            if want_np { b.port.or(Some(0)) } else { None },
        )
        .await?;

    st.audit(
        &user,
        "session.sql-gateway",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("deployment={gw_name} port={port:?}")),
        None,
    );

    Ok(Json(json!({
        "ok": true,
        "deployment": gw_name,
        "service": gw_name,
        "nodePort": port,
        "url": port.map(|p| format!("http://<node>:{}", p)),
        "note": "镜像里必须有 bin/sql-gateway.sh；Pod 起来后可查 /v1/info 验证",
    })))
}
