//! 环境基线（Profile）：读列表 / 覆盖保存。
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_core as core;
use flinkops_types::{Error, Finding, Profile};
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

pub async fn list(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "items": st.store.list_profiles().await? })))
}

/// 保存基线；同时返回用该基线生成一份 spec 的校验结果（前端预览用）。
///
/// 有 error 级 finding 时**拒绝入库**：基线是所有新建作业的公共来源，
/// 一条坏基线（空镜像 / 把 id 填成 image）会污染整个团队的提交链路。
pub async fn save(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
    Json(mut p): Json<Profile>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    p.id = id;

    // 1) 基线自身字段校验
    let mut field_findings: Vec<Finding> = Vec::new();
    if p.id.trim().is_empty() || p.id.len() > 63 {
        field_findings.push(Finding::error(
            "PROFILE_ID_INVALID",
            "id",
            "基线 ID 需 1–63 字符",
        ));
    }
    if p.image.trim().is_empty() {
        field_findings.push(Finding::error("IMAGE_REQUIRED", "image", "镜像不能为空"));
    } else if !p.image.contains(':') && !p.image.contains('@') {
        field_findings.push(Finding::error(
            "IMAGE_INVALID",
            "image",
            format!("镜像需要带 tag（如 flink:1.15.4），当前为 {}", p.image),
        ));
    }
    if p.namespace.trim().is_empty() {
        field_findings.push(Finding::error(
            "NAMESPACE_REQUIRED",
            "namespace",
            "命名空间不能为空",
        ));
    }
    if !p.flink_version.starts_with("v1_") {
        field_findings.push(Finding::error(
            "VERSION_INVALID",
            "flinkVersion",
            format!("flinkVersion 形如 v1_19，当前为 {}", p.flink_version),
        ));
    }
    if field_findings
        .iter()
        .any(|f| f.severity == flinkops_types::Severity::Error)
    {
        return Err(ApiError(Error::BadRequest(format!(
            "基线不合法：{}",
            field_findings
                .iter()
                .map(|f| format!("{}@{}: {}", f.code, f.path, f.message))
                .collect::<Vec<_>>()
                .join("; ")
        ))));
    }

    // 2) 用基线渲染一份 spec 再校验（配置项层面）
    p.builtin = false;
    let preview = core::spec_from_profile(&p, &json!({}));
    // 基线只描述公共配置（镜像/版本/资源/路径），不含具体作业：
    // 预览里必然缺 jarURI/sql、也必然没有 metadata，这几条对基线不适用。
    let findings: Vec<_> = core::validate(&preview)
        .into_iter()
        .filter(|f| f.code != "JOB_SOURCE_REQUIRED")
        .collect();
    if core::has_errors(&findings) {
        return Err(ApiError(Error::BadRequest(format!(
            "基线渲染出的 spec 有错误：{}",
            findings
                .iter()
                .filter(|f| f.severity == flinkops_types::Severity::Error)
                .map(|f| format!("{}@{}: {}", f.code, f.path, f.message))
                .collect::<Vec<_>>()
                .join("; ")
        ))));
    }

    st.store.save_profile(&p).await?;
    st.audit(&user, "profile.save", &p.id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true, "findings": findings })))
}
