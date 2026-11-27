//! 为作业创建 Ingress（域名直连 Flink UI / REST）。

#![allow(unused_imports)]
use super::super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressBody {
    pub host: String,
    #[serde(default)]
    pub class_name: Option<String>,
}

/// POST /api/jobs/{c}/{ns}/{n}/ingress —— 创建工作 Ingress（域名直连）
pub async fn create_ingress(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    w: RequireWrite,
    j: JobTarget,
    Json(body): Json<IngressBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;

    let host = body.host.trim().to_string();
    if host.is_empty() {
        return Err(ApiError(Error::BadRequest("host 不能为空".into())));
    }
    // 常见手误：贴了 URL（https://x/）、带空格、带端口
    if host.contains("://") || host.contains('/') || host.contains(' ') {
        return Err(ApiError(Error::BadRequest(format!(
            "host 只填域名（如 flink-orders.example.com），不要带协议/路径/空格，收到 {host}"
        ))));
    }
    if !host.contains('.')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(ApiError(Error::BadRequest(format!(
            "host 不像合法域名：{host}"
        ))));
    }
    gate(
        &st,
        &headers,
        "flinkops_create_ingress",
        &ns,
        &name,
        json!({ "host": body.host }),
        true,
    )
    .await?;
    // 先确认作业存在，避免给不存在的作业建 Ingress
    let c = st.cluster(&cluster).await?;
    let _ = c.get_raw(crd::KIND_DEPLOYMENT, &ns, &name).await?;

    let class = body
        .class_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "traefik".into());
    if class.len() > 63
        || !class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err(ApiError(Error::BadRequest(format!(
            "ingressClassName 不合法：{class}"
        ))));
    }
    // 名字带平台后缀：operator 会按 spec.ingress.template 生成 `{name}-rest`，
    // 用同名会互相抢字段（域名时通时不通）。
    let ing_name = format!("{name}-rest-flinkops");
    let svc = format!("{name}-rest");
    c.apply_ingress(&ns, &ing_name, body.host.trim(), &svc, 8081, &class)
        .await?;
    st.audit(
        &user,
        "job.ingress",
        &name,
        &cluster,
        &ns,
        "ok",
        Some(format!("host={} class={class}", body.host.trim())),
        None,
    );
    Ok(Json(json!({
        "ok": true,
        "ingress": ing_name,
        "host": host,
        // 平台只建 HTTP 规则，不建 TLS；再回一个 https:// 链接等于骗人。
        "url": format!("http://{host}"),
        "note": "未启用 TLS（需要证书请自行给 Ingress 加 tls 段）；把 Flink UI 暴露到域名前请先在网关侧挂鉴权",
    })))
}
