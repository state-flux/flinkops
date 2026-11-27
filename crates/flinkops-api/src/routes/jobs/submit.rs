//! 提交与更新：dry-run 预检、创建、PUT 更新（带 resourceVersion CAS）。

// 子模块通过父模块拿到全部共享 import 与 helper（child 可见父的私有项）
#![allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub yaml: Option<String>,
    pub object: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateQuery {
    /// 多集群时必填；只有一个集群时可省略
    pub cluster: Option<String>,
    #[serde(default, rename = "dryRun")]
    pub dry_run: bool,
}

/// POST /api/jobs —— 提交一个 FlinkDeployment。
///
/// 顺序固定：解析 → 校验（core）→ 重名检查 → apply → 审计。
/// `dryRun=true` 时只做前三步，供前端预检与联调用。
pub async fn create(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    Query(q): Query<CreateQuery>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;

    let mut value: Value = match (body.object, body.yaml) {
        (Some(v), _) => v,
        (None, Some(y)) => serde_yaml::from_str(&y)
            .map_err(|e| ApiError(Error::BadRequest(format!("YAML 解析失败: {e}"))))?,
        _ => {
            return Err(ApiError(Error::BadRequest(
                "需要 yaml 或 object 之一".into(),
            )))
        }
    };

    hoist_spec_metadata(&mut value);
    strip_server_fields(&mut value);
    let kind = crd::s(&value, &["kind"]).unwrap_or_default();
    if kind != crd::KIND_DEPLOYMENT {
        return Err(ApiError(Error::BadRequest(format!(
            "提交内容不是合法的 {}（缺少 kind 或写错），收到 {kind:?}；请用 /api/render 重新生成 manifest",
            crd::KIND_DEPLOYMENT
        ))));
    }

    let ns = crd::s(&value, &["metadata", "namespace"])
        .ok_or_else(|| ApiError(Error::BadRequest("metadata.namespace 不能为空".into())))?;
    let name = crd::s(&value, &["metadata", "name"])
        .ok_or_else(|| ApiError(Error::BadRequest("metadata.name 不能为空".into())))?;
    // namespace 白名单：路径里没有 ns（create 的 ns 在 body 里），中间件看不到，
    // 必须在这里显式校验
    if let Some(p) = st.principal(&headers).await {
        if !p.ns_ok(&ns) {
            return Err(ApiError(Error::Forbidden(format!(
                "{} 无权在命名空间 {ns} 创建作业",
                p.user
            ))));
        }
    }

    // 集群：显式指定 > 唯一集群
    let cluster_id = match q.cluster.clone() {
        Some(c) => c,
        None if st.clusters_count().await == 1 => st
            .cluster_ids()
            .await
            .into_iter()
            .next()
            .unwrap_or_default(),
        None => {
            return Err(ApiError(Error::BadRequest(format!(
                "已接入 {} 个集群，请用 ?cluster= 指定目标",
                st.clusters_count().await
            ))))
        }
    };

    // 1) 校验：完整对象（metadata + spec）+ 集群侧引用（ns / serviceAccount 是否存在）
    let env = core::Env {
        in_cluster: st
            .cluster(&cluster_id)
            .await
            .map(|c| c.in_cluster)
            .unwrap_or(true),
    };
    let mut findings = core::validate_object_with_env(&value, env);
    findings
        .extend(crate::routes::editor::cluster_ref_findings(&st, Some(&cluster_id), &value).await);
    if core::has_errors(&findings) {
        return Ok(bad_request_with_findings("校验未通过", &findings));
    }

    // 2) 闸门：MCP Token 需要 admin scope + 一次性审批（建作业可提交特权 Pod）
    //    预检（dryRun）不落库、可重复，因此不消耗一次性审批。
    if !q.dry_run {
        gate(
            &st,
            &headers,
            "flinkops_create_job",
            &ns,
            &name,
            json!({ "spec": value.get("spec") }),
            true,
        )
        .await?;
    }

    // 3) 重名检查
    let c = st.cluster(&cluster_id).await?;
    if c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await.is_ok() {
        return Err(ApiError(Error::Conflict(format!("{ns}/{name} 已存在"))));
    }

    // 4) 提交（dryRun 交给 apiserver 做真实校验：CRD schema + webhook，不落库）
    if q.dry_run {
        if let Err(e) = c
            .create_object_dryrun(crd::KIND_DEPLOYMENT, &ns, value.clone())
            .await
        {
            let msg = format!("服务端 dry-run 拒绝：{}", e.message());
            // 预检失败也要留痕（action 与真实提交区分开）
            st.audit(
                &user,
                "job.validate",
                &name,
                &cluster_id,
                &ns,
                "error",
                Some(msg.clone()),
                None,
            );
            return Err(ApiError(Error::BadRequest(msg)));
        }
    } else {
        c.create_object(crd::KIND_DEPLOYMENT, &ns, value.clone())
            .await?;
        refresh_index(&st, &cluster_id, &ns, &name).await;
    }

    let spec = value.get("spec").cloned().unwrap_or(Value::Null);
    let detail = format!(
        "image={} parallelism={}",
        crd::s(&spec, &["image"]).unwrap_or_default(),
        crd::i(&spec, &["job", "parallelism"]).unwrap_or(0)
    );
    st.audit(
        &user,
        // 预检不是"提交成功"，审计里必须分得清（否则事后追溯会看到假提交）
        if q.dry_run {
            "job.validate"
        } else {
            "job.submit"
        },
        &name,
        &cluster_id,
        &ns,
        "ok",
        Some(if q.dry_run {
            format!("dryRun {detail}")
        } else {
            detail
        }),
        None,
    );

    Ok(Json(json!({
        "ok": true,
        "name": name,
        "namespace": ns,
        "cluster": cluster_id,
        "dryRun": q.dry_run,
    }))
    .into_response())
}

/// PUT /api/jobs/{c}/{ns}/{n} —— 保存编辑后的 CRD（整体 replace + resourceVersion CAS）
///
/// 编辑器原来只能"复制 kubectl apply"，变更不经过平台、不进审计、也不受只读/
/// Token 范围约束。这里提供受控的保存入口。
pub async fn update_job(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    let mut value: Value = match (body.object, body.yaml) {
        (Some(v), _) => v,
        (None, Some(y)) => serde_yaml::from_str(&y)
            .map_err(|e| ApiError(Error::BadRequest(format!("YAML 解析失败: {e}"))))?,
        _ => {
            return Err(ApiError(Error::BadRequest(
                "需要 yaml 或 object 之一".into(),
            )))
        }
    };

    hoist_spec_metadata(&mut value);
    let kind = crd::s(&value, &["kind"]).unwrap_or_default();
    if kind != crd::KIND_DEPLOYMENT && kind != crd::KIND_SESSION_JOB {
        return Err(ApiError(Error::BadRequest(format!(
            "只支持 {} / {}，收到 {kind:?}",
            crd::KIND_DEPLOYMENT,
            crd::KIND_SESSION_JOB
        ))));
    }
    // 路径与内容必须一致：防止"编辑 A 保存成 B"
    let body_name = crd::s(&value, &["metadata", "name"]).unwrap_or_default();
    let body_ns = crd::s(&value, &["metadata", "namespace"]).unwrap_or_default();
    if body_name != name || body_ns != ns {
        return Err(ApiError(Error::BadRequest(format!(
            "请求路径是 {ns}/{name}，但内容里是 {body_ns}/{body_name}"
        ))));
    }

    let env = core::Env {
        in_cluster: st
            .cluster(&cluster)
            .await
            .map(|c| c.in_cluster)
            .unwrap_or(true),
    };
    let mut findings = core::validate_object_with_env(&value, env);
    findings.extend(crate::routes::editor::cluster_ref_findings(&st, Some(&cluster), &value).await);
    if core::has_errors(&findings) {
        return Ok(bad_request_with_findings("校验未通过", &findings));
    }

    // 整对象 replace 前必须剥掉服务端字段：
    // /yaml 会把 status 一起导出来，原样写回时 operator 的 mutation webhook 会在
    // 反序列化 status.lifecycleState 时抛 500（"Could not deserialize spec"）。
    if let Some(obj) = value.as_object_mut() {
        obj.remove("status");
        if let Some(meta) = obj.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            for k in [
                "managedFields",
                "uid",
                "creationTimestamp",
                "generation",
                "selfLink",
            ] {
                meta.remove(k);
            }
            if let Some(ann) = meta.get_mut("annotations").and_then(|a| a.as_object_mut()) {
                ann.remove("kubectl.kubernetes.io/last-applied-configuration");
            }
        }
    }

    // 打码占位符绝对不能写回集群：那会把凭据替换成 `****`（真实踩过）。
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.contains("****") || serialized.contains("<omitted") {
        return Err(ApiError(Error::BadRequest(
            "提交内容含打码占位符（**** / <omitted>）。请用未打码的 YAML 保存，\
             或在编辑器里点「显示明文」重新加载后再改"
                .into(),
        )));
    }

    gate(
        &st,
        &headers,
        "flinkops_update_job",
        &ns,
        &name,
        json!({ "spec": value.get("spec") }),
        true,
    )
    .await?;

    let c = st.cluster(&cluster).await?;
    let current = c.get_raw(&kind, &ns, &name).await?;
    let current_rv = crd::s(&current, &["metadata", "resourceVersion"]).unwrap_or_default();
    // 真 CAS：优先用客户端带来的基线版本（If-Match 头，或编辑器里那份 YAML 的
    // metadata.resourceVersion）。都没有才退回"刚读到的 RV"（等价 last-write-wins，
    // 但至少不会再假装自己有 CAS）。
    let client_rv = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| crd::s(&value, &["metadata", "resourceVersion"]).filter(|s| !s.is_empty()));
    let rv = match client_rv {
        Some(rv) => {
            if !current_rv.is_empty() && rv != current_rv {
                return Err(ApiError(Error::Conflict(format!(
                    "{ns}/{name} 已被他人修改（基线版本 {rv}，当前 {current_rv}），请重新加载后再保存"
                ))));
            }
            rv
        }
        None => current_rv,
    };
    value["metadata"]["resourceVersion"] = json!(rv);

    // 审计里记"改了哪些字段"，不记整份 spec（可能含凭据）
    let diffs = core::diff(
        &current.get("spec").cloned().unwrap_or(Value::Null),
        &value.get("spec").cloned().unwrap_or(Value::Null),
    );
    let changed: Vec<String> = diffs.iter().take(20).map(|d| d.path.clone()).collect();

    c.replace_object(&kind, &ns, &name, value).await?;
    refresh_index(&st, &cluster, &ns, &name).await;
    st.audit(
        &user,
        "job.update",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("changed={}", changed.join(","))),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "name": name,
        "namespace": ns,
        "changed": diffs.len(),
        "paths": changed,
    }))
    .into_response())
}
