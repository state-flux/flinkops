//! 蓝绿发布：新建一套 green 作业，验收后切换。

#![allow(unused_imports)]
use super::super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlueGreenBody {
    /// 目标镜像（不填则保持当前镜像，只做一次无变更的蓝绿演练）
    pub image: Option<String>,
    /// 蓝绿部署 CR 的名字（默认 <job>-bg）
    pub name: Option<String>,
    /// 旧部署保留时长后再删（如 "5m"）
    pub deletion_delay: Option<String>,
    /// 切换后是否保留旧（蓝色）部署
    #[serde(default)]
    pub keep_blue: bool,
}

/// POST /api/jobs/{c}/{ns}/{n}/blue-green —— 创建 FlinkBlueGreenDeployment
///
/// 与「滚动升级」的区别：蓝绿不会 cancel 旧作业，而是起绿色集群、
/// 从蓝色 savepoint 恢复、健康后再切流量，最后延时删蓝色。
pub async fn create_blue_green(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<BlueGreenBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    gate(
        &st,
        &headers,
        "flinkops_blue_green",
        &ns,
        &name,
        json!({ "image": body.image, "name": body.name }),
        true,
    )
    .await?;
    let c = st.cluster(&cluster).await?;
    let cur = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;

    // 只有 application 模式（有 job 段）才能蓝绿升级
    if cur.get("spec").and_then(|s| s.get("job")).is_none() {
        return Err(ApiError(Error::BadRequest(format!(
            "{name} 是 session cluster（无 job 段），蓝绿升级只适用于 application 作业"
        ))));
    }

    // 以当前 spec 为基线，只覆盖镜像
    let mut spec = cur.get("spec").cloned().unwrap_or(json!({}));
    if let Some(img) = &body.image {
        if img.trim().is_empty() {
            return Err(ApiError(Error::BadRequest("image 不能为空字符串".into())));
        }
        spec["image"] = json!(img.trim());
    }
    // 蓝绿升级必须能从 savepoint 恢复，否则等于重启
    spec["job"]["upgradeMode"] = json!("savepoint");

    let bg_name = body.name.clone().unwrap_or_else(|| format!("{name}-bg"));
    let bg_name: String = bg_name
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

    // 名字必须能当 k8s 资源名（小写字母/数字/-，≤63，首尾是字母数字）
    if bg_name.len() > 63
        || bg_name.is_empty()
        || bg_name.starts_with('-')
        || bg_name.ends_with('-')
        || !bg_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ApiError(Error::BadRequest(format!(
            "蓝绿部署名不合法（需 DNS-1123 label）：{bg_name}"
        ))));
    }

    let mut configuration = serde_json::Map::new();
    // keepBlue 与 deletionDelay 不再互相覆盖：显式给了 delay 就用它，
    // keepBlue 只在没给 delay 时兜底成 24h。
    let delay = body
        .deletion_delay
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            if body.keep_blue {
                Some("24h".to_string())
            } else {
                None
            }
        });
    if let Some(d) = delay {
        configuration.insert(
            "kubernetes.operator.bluegreen.deployment-deletion.delay".into(),
            json!(d),
        );
    }

    let obj = json!({
        "apiVersion": "flink.apache.org/v1beta1",
        "kind": crd::KIND_BLUE_GREEN,
        "metadata": {
            "name": bg_name,
            "namespace": ns,
            "labels": { "flinkops.io/managed-by": "flinkops", "flinkops.io/blue-green-of": name }
        },
        "spec": {
            "configuration": configuration,
            "template": {
                "metadata": { "name": name },
                "spec": spec
            }
        }
    });

    c.apply_custom(
        "flink.apache.org",
        "v1beta1",
        crd::KIND_BLUE_GREEN,
        &ns,
        obj,
    )
    .await?;

    st.audit(
        &user,
        "job.blue-green",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("bg={bg_name} image={:?}", body.image)),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "blueGreen": bg_name,
        "target": name,
        "forcedUpgradeMode": "savepoint",
        "note": "operator 会起绿色集群、从蓝色 savepoint 恢复、健康后切流量；模板里的 job.upgradeMode 被强制为 savepoint（蓝绿必须靠快照迁移状态）",
    })))
}

/// GET /api/jobs/{c}/{ns}/{n}/blue-green —— 查该作业的蓝绿部署状态
pub async fn blue_green_status(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    j: JobTarget,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let JobTarget { cluster, ns, name } = j;
    let c = st.cluster(&cluster).await?;
    let items = c.list_raw(crd::KIND_BLUE_GREEN, Some(&ns)).await?;
    let mut out = Vec::new();
    for o in items {
        let of = o
            .pointer("/metadata/labels/flinkops.io~1blue-green-of")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if of != name {
            continue;
        }
        out.push(json!({
            "name": o.pointer("/metadata/name").and_then(|v| v.as_str()),
            "state": o.pointer("/status/state").and_then(|v| v.as_str()),
            "jobStatus": o.pointer("/status/jobStatus"),
            "error": o.pointer("/status/error").and_then(|v| v.as_str()),
            "createdAt": o.pointer("/metadata/creationTimestamp").and_then(|v| v.as_str()),
        }));
    }
    Ok(Json(json!({ "items": out })))
}
