//! 作业模板：把一套可复用的配置（镜像 / jar / 参数 / 资源配置）存下来。
//!
//! 安全前提：生成模板时**只保留白名单字段**，绝不把 podTemplate 里的
//! initContainer env（常含 AK/SK）写进库。
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_types::{crd, Error};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

pub async fn list(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    let items = st.store.list_templates().await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub name: String,
    /// 直接给 payload（手写模板）
    pub payload: Option<Value>,
    /// 或从现有作业生成（只取白名单字段）
    pub from_job: Option<FromJob>,
}

#[derive(Deserialize)]
pub struct FromJob {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
}

/// 白名单：只保留这几段，且过滤敏感键。
fn sanitize(spec: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in ["image", "flinkVersion", "serviceAccount", "mode"] {
        if let Some(v) = spec.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    // jobManager/taskManager 只留 replicas 与 resource：
    // 它们的 podTemplate 里常塞 initContainer 的 AK/SK（真实踩过），绝不进模板。
    for key in ["jobManager", "taskManager"] {
        if let Some(v) = spec.get(key) {
            let mut m = serde_json::Map::new();
            for k in ["replicas", "resource"] {
                if let Some(x) = v.get(k) {
                    m.insert(k.to_string(), x.clone());
                }
            }
            out.insert(key.to_string(), Value::Object(m));
        }
    }
    if let Some(job) = spec.get("job") {
        let mut j = serde_json::Map::new();
        for k in [
            "jarURI",
            "entryClass",
            "args",
            "parallelism",
            "upgradeMode",
            "sql",
        ] {
            if let Some(v) = job.get(k) {
                j.insert(k.to_string(), v.clone());
            }
        }
        out.insert("job".into(), Value::Object(j));
    }
    if let Some(Value::Object(cfg)) = spec.get("flinkConfiguration") {
        let filtered: serde_json::Map<String, Value> = cfg
            .iter()
            .filter(|(k, _)| {
                let lower = k.to_ascii_lowercase();
                !(lower.contains("password")
                    || lower.contains("secret")
                    || lower.contains("access.key")
                    || lower.contains("token"))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.insert("flinkConfiguration".into(), Value::Object(filtered));
    }
    Value::Object(out)
}

pub async fn create(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if body.name.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("模板名不能为空".into())));
    }

    let (payload, source_job) = match (&body.payload, &body.from_job) {
        (Some(p), _) => (p.clone(), None),
        (None, Some(fj)) => {
            let obj = st
                .cluster(&fj.cluster)
                .await?
                .get_raw(crd::KIND_DEPLOYMENT, &fj.namespace, &fj.name)
                .await?;
            let spec = obj.get("spec").cloned().unwrap_or(Value::Null);
            (
                sanitize(&spec),
                Some(format!("{}/{}/{}", fj.cluster, fj.namespace, fj.name)),
            )
        }
        _ => {
            return Err(ApiError(Error::BadRequest(
                "需要 payload 或 fromJob 之一".into(),
            )))
        }
    };

    let id = uuid::Uuid::new_v4().to_string();
    let t = json!({
        "id": id,
        "name": body.name.trim(),
        "kind": "FlinkDeployment",
        "sourceJob": source_job,
        "payload": payload,
    });
    st.store.save_template(&t).await?;
    st.audit(
        &user,
        "template.save",
        t["name"].as_str().unwrap_or_default(),
        "",
        "",
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "template": t })))
}

pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.delete_template(&id).await?;
    st.audit(&user, "template.delete", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub name: Option<String>,
    pub payload: Option<Value>,
}

/// PUT /api/templates/{id} —— 改名 / 改 payload
pub async fn update(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    let mut items = st.store.list_templates().await?;
    let mut t = items
        .iter_mut()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        .cloned()
        .ok_or_else(|| ApiError(Error::NotFound(format!("模板 {id} 不存在"))))?;

    if let Some(n) = &body.name {
        if n.trim().is_empty() {
            return Err(ApiError(Error::BadRequest("模板名不能为空".into())));
        }
        t["name"] = json!(n.trim());
    }
    if let Some(p) = body.payload {
        t["payload"] = p;
    }
    st.store.save_template(&t).await?;
    st.audit(
        &user,
        "template.update",
        t["name"].as_str().unwrap_or_default(),
        "",
        "",
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true, "template": t })))
}
