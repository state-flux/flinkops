//! CRD 编辑器支撑：YAML 读取 / 校验 / diff。
//!
//! 前端仍负责编辑体验（CodeMirror），但 **YAML 解析与校验只在 Rust 里做一份**，
//! 避免"浏览器里能过、推到集群被拒"的不一致。
use std::sync::Arc;

use axum::extract::{Path, Query, State};

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use flinkops_core as core;
use flinkops_types::{crd, Error, Finding, SpecDiff};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;

use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YamlBody {
    /// 完整 CRD 或只有 spec 的 YAML
    pub yaml: String,
}

#[derive(Deserialize, Default)]
pub struct ValidateQuery {
    /// 指定集群时额外校验 namespace / serviceAccount 是否存在
    pub cluster: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

/// 校验一段 YAML：解析 → 取 spec → core::validate。
/// 同时把解析后的 spec 返回，前端可以接着用它做 diff / 提交。
pub async fn validate(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Query(q): Query<ValidateQuery>,
    Json(body): Json<YamlBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let value: Value = serde_yaml::from_str(&body.yaml)
        .map_err(|e| ApiError(Error::BadRequest(format!("YAML 解析失败: {e}"))))?;

    // 兼容三种输入：完整 CRD / {spec:{...}} / 裸 spec
    let spec = value.get("spec").cloned().unwrap_or_else(|| value.clone());
    // 平台与被校验集群是否同一网络：决定"改 ClusterIP 走反代"这条建议是否成立
    let env = core::Env {
        in_cluster: match q.cluster.as_deref() {
            Some(id) => st.cluster(id).await.map(|c| c.in_cluster).unwrap_or(true),
            None => true,
        },
    };
    let mut findings: Vec<Finding> = if value.get("spec").is_some() {
        core::validate_object_with_env(&value, env)
    } else {
        core::validate_with_env(&spec, env)
    };
    findings.extend(cluster_ref_findings(&st, q.cluster.as_deref(), &value).await);

    Ok(Json(json!({
        "findings": findings,
        "spec": spec,
        "hasErrors": core::has_errors(&findings),
    })))
}

/// 集群侧引用校验：namespace 是否存在、serviceAccount 是否存在。
///
/// 这类错误在 apiserver / operator 侧才会暴露（404、Pod FailedCreate），
/// 提前到校验阶段，避免"平台校验通过、集群起不来"。
pub(crate) async fn cluster_ref_findings(
    st: &Arc<AppState>,
    cluster: Option<&str>,
    obj: &Value,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let id = match cluster {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => match st.cluster_ids().await.into_iter().next() {
            Some(c) => c,
            None => return out,
        },
    };
    let Ok(c) = st.cluster(&id).await else {
        return out;
    };
    let ns = obj
        .pointer("/metadata/namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if ns.is_empty() {
        return out;
    }
    match c.namespace_exists(&ns).await {
        Ok(false) => out.push(Finding::error(
            "NAMESPACE_NOT_FOUND",
            "metadata.namespace",
            format!("集群 {id} 里没有命名空间 {ns}"),
        )),
        Ok(true) => {
            if let Some(sa) = obj
                .pointer("/spec/serviceAccount")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Ok(list) = c.list_service_accounts(&ns).await {
                    if !list.iter().any(|x| x == sa) {
                        out.push(Finding::error(
                            "SERVICE_ACCOUNT_NOT_FOUND",
                            "spec.serviceAccount",
                            format!(
                                "命名空间 {ns} 里没有 serviceAccount {sa}（已有：{}）",
                                list.join(", ")
                            ),
                        ));
                    }
                }
            }
        }
        Err(_) => {}
    }
    out
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffBody {
    pub before: Value,
    pub after: Value,
}

/// 比较两份 spec（点号路径）。前端用它渲染"与集群的差异"。
pub async fn diff(
    _st: State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<DiffBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let diffs: Vec<SpecDiff> = core::diff(&body.before, &body.after);
    Ok(Json(json!({ "diffs": diffs, "count": diffs.len() })))
}

/// 把某个作业的 CRD 原样导出成 YAML（编辑器"加载"用）。
///
/// 打码逻辑已下沉到 `crd::redact_sensitive`（详情接口复用同一套）。
#[allow(dead_code)]
fn redact(v: &mut Value) {
    const NEEDLES: [&str; 6] = [
        "ACCESS_KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "PRIVATE_KEY",
    ];

    fn looks_sensitive(name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        NEEDLES.iter().any(|n| upper.contains(n))
    }

    match v {
        Value::Object(map) => {
            // initContainer 形态：{name: "AWS_SECRET_ACCESS_KEY", value: "xxx"}
            let key_name = map
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let is_env_entry = looks_sensitive(&key_name);
            for (k, val) in map.iter_mut() {
                // operator 把整段 spec 存成字符串（含 initContainer 的 AK/SK）：省略
                if k == "lastReconciledSpec" || k == "lastStableSpec" {
                    *val = Value::String("<omitted: 含完整 spec 与凭据>".into());
                    continue;
                }
                if is_env_entry && k == "value" && val.is_string() {
                    *val = Value::String("****".into());
                    continue;
                }
                if looks_sensitive(k) && val.is_string() {
                    *val = Value::String("****".into());
                    continue;
                }
                // kubectl 的 last-applied-configuration 是整份对象的 JSON 字符串，
                // 里面同样带 initContainer 的 AK/SK
                if val.is_string() {
                    let sv = val.as_str().unwrap_or("");
                    if k.contains("last-applied-configuration")
                        || sv.contains("ACCESS_KEY")
                        || sv.contains("SECRET_ACCESS_KEY")
                        || sv.contains("PRIVATE KEY")
                    {
                        *val = Value::String("<omitted: 含完整对象与凭据>".into());
                        continue;
                    }
                }
                redact(val);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

/// 去掉服务端生成、写回会出错的字段（编辑器视图用）。
fn strip_server_fields(obj: &mut Value) {
    if let Some(map) = obj.as_object_mut() {
        map.remove("status");
        if let Some(meta) = map.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            for k in [
                "managedFields",
                "uid",
                "creationTimestamp",
                "generation",
                "selfLink",
                // 注意：**保留 resourceVersion** —— 编辑器的"保存到集群"要用它做 CAS，
                // 剥掉之后后端只能退回"刚读到的 RV"，等于没有并发保护。
            ] {
                meta.remove(k);
            }
            if let Some(ann) = meta.get_mut("annotations").and_then(|a| a.as_object_mut()) {
                ann.remove("kubectl.kubernetes.io/last-applied-configuration");
            }
        }
    }
}

pub async fn job_yaml(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path((cluster, ns, name)): Path<(String, String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Response> {
    let AuthUser { .. } = u;
    // 默认打码：CR 里常有 initContainer 的 AK/SK，避免截图/分享泄露
    let do_redact = q.get("redact").map(|v| v != "false").unwrap_or(true);
    // 默认剥掉服务端字段：managedFields 动辄上百行，把 spec 顶到屏幕外，
    // 而且复制出去 apply 会报错。`?raw=true` 保留原始形态。
    let keep_raw = q.get("raw").map(|v| v == "true").unwrap_or(false);
    // SessionJob 也要能看 YAML（原来硬编码 FlinkDeployment → 404）
    let (_kind, mut obj) = st.cluster(&cluster).await?.get_any(&ns, &name).await?;
    if do_redact {
        crd::redact_sensitive(&mut obj);
    }
    if !keep_raw {
        strip_server_fields(&mut obj);
    }
    let text = serde_yaml::to_string(&obj)
        .map_err(|e| ApiError(Error::Internal(format!("序列化 YAML 失败: {e}"))))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        text,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct FormatBody {
    pub yaml: String,
}

/// 纯排版：解析 → 重新序列化，**不经过基线渲染**。
///
/// 早期"格式化"复用 render（基线 + 覆盖项），会把 apiVersion/kind/metadata 丢掉、
/// 还把基线默认键倒灌进用户文档，属于"一键毁配置"。
pub async fn format(
    _st: State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<FormatBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
    let value: Value = serde_yaml::from_str(&body.yaml)
        .map_err(|e| ApiError(Error::BadRequest(format!("YAML 解析失败: {e}"))))?;
    let text = serde_yaml::to_string(&value)
        .map_err(|e| ApiError(Error::Internal(format!("序列化 YAML 失败: {e}"))))?;
    Ok(Json(json!({ "yaml": text })))
}

/// 用基线 + 覆盖项生成 spec（提交向导与编辑器共用）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderBody {
    pub profile_id: Option<String>,
    /// 缺省必须是空对象：`Value::default()` 是 Null，会把整份 spec 覆盖掉
    #[serde(default = "empty_object")]
    pub overrides: Value,
}

/// 生成**可直接 apply 的完整 manifest**（apiVersion/kind/metadata/spec）。
///
/// 早期版本只返回裸 spec，前端拿去 POST /api/jobs 必然被
/// "只支持 FlinkDeployment" 拒掉；这里补齐外壳，并保留 `spec` 字段给
/// 已有的 diff / findings 渲染继续使用。
pub async fn render(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<RenderBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { .. } = u;
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
            .ok_or_else(|| ApiError(Error::NotFound("没有任何基线".into())))?,
    };

    let mut spec = core::spec_from_profile(&profile, &body.overrides);
    // 覆盖项里的 `metadata` 会被 spec_from_profile 深合并进 spec，生成
    // 非法的 `spec.metadata`（FlinkDeployment CRD 没有这个字段，apiserver 静默裁剪）。
    // 这里把它取出来，稍后合并到顶层 metadata。
    let stray_meta = spec
        .as_object_mut()
        .and_then(|m| m.remove("metadata"))
        .unwrap_or_else(|| serde_json::json!({}));

    // metadata：覆盖项优先，缺省用基线里的命名空间
    let meta_in = body
        .overrides
        .get("metadata")
        .cloned()
        .unwrap_or_else(empty_object);
    let name = meta_in
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ns = meta_in
        .get("namespace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&profile.namespace)
        .to_string();
    let mut labels = profile.labels.clone();
    if let Some(extra) = stray_meta.get("labels").and_then(|v| v.as_object()) {
        for (k, v) in extra {
            if let Some(s) = v.as_str() {
                labels.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(Value::Object(extra)) = meta_in.get("labels") {
        for (k, v) in extra {
            if let Some(s) = v.as_str() {
                labels.insert(k.clone(), s.to_string());
            }
        }
    }

    let manifest = json!({
        "apiVersion": "flink.apache.org/v1beta1",
        "kind": "FlinkDeployment",
        "metadata": { "name": name, "namespace": ns, "labels": labels },
        "spec": spec,
    });

    let mut findings = core::validate_object(&manifest);
    findings.extend(cluster_ref_findings(&st, None, &manifest).await);

    Ok(Json(json!({
        "spec": spec,
        "manifest": manifest,
        "yaml": serde_yaml::to_string(&manifest).unwrap_or_default(),
        "findings": findings,
        "hasErrors": core::has_errors(&findings),
    })))
}
