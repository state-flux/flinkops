//! 部署模板：把「产出 FlinkDeployment 的 YAML」从代码里抽出来，开源用户可改。
//!
//! 模板是带 `{{placeholder}}` 的完整 YAML；渲染时用变量替换后解析。
//! 内置两个默认模板（sql-app / sql-sessionjob），用户可另存自定义版本。
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};

use axum::Json;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

/// 内置模板：SQL 作为 application 部署（FlinkDeployment + sql-runner）
pub const BUILTIN_SQL_APP: &str = r#"apiVersion: flink.apache.org/v1beta1
kind: FlinkDeployment
metadata:
  name: "{{name}}"
  namespace: "{{namespace}}"
  labels:
    flinkops.io/managed-by: flinkops
spec:
  image: "{{image}}"
  flinkVersion: "{{flinkVersion}}"
  mode: native
  serviceAccount: "{{serviceAccount}}"
  flinkConfiguration:
    taskmanager.numberOfTaskSlots: "{{slots}}"
    restart-strategy.type: "{{restartStrategy}}"
    state.checkpoints.dir: "{{checkpointDir}}"
    state.savepoints.dir: "{{savepointDir}}"
  jobManager:
    replicas: 1
    resource:
      cpu: {{jmCpu}}
      memory: "{{jmMemory}}"
  taskManager:
    replicas: {{tmReplicas}}
    resource:
      cpu: {{tmCpu}}
      memory: "{{tmMemory}}"
  job:
    jarURI: "{{runnerJar}}"
    args: {{args}}
    parallelism: {{parallelism}}
    upgradeMode: "{{upgradeMode}}"
  podTemplate:
    apiVersion: v1
    kind: Pod
    metadata:
      name: pod-template
    spec:
      containers:
        - name: flink-main-container
          volumeMounts:
            - name: sql-scripts
              mountPath: /opt/flink/usrlib/sql-scripts
      volumes:
        - name: sql-scripts
          configMap:
            name: "{{sqlConfigMap}}"
"#;

/// 内置模板：SQL 作为 FlinkSessionJob 提交到会话集群
pub const BUILTIN_SQL_SESSIONJOB: &str = r#"apiVersion: flink.apache.org/v1beta1
kind: FlinkSessionJob
metadata:
  name: "{{name}}"
  namespace: "{{namespace}}"
  labels:
    flinkops.io/managed-by: flinkops
spec:
  deploymentName: "{{sessionCluster}}"
  job:
    jarURI: "{{runnerJar}}"
    args: {{args}}
    parallelism: {{parallelism}}
    upgradeMode: "{{upgradeMode}}"
"#;

/// 用 `{{k}}` 替换渲染。值必须是「已按 YAML 语义格式化好的字符串」：
/// 字符串要自带引号、数组是 JSON、数字裸写。
pub fn render(template: &str, vars: &HashMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// 把 `{{x}}` 全替换成裸标识符 `PLACEHOLDER`（用于语法校验）
fn strip_placeholders(tpl: &str) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(s) = rest.find("{{") {
        out.push_str(&rest[..s]);
        match rest[s + 2..].find("}}") {
            Some(e) => {
                out.push_str("PLACEHOLDER");
                rest = &rest[s + 2 + e + 2..];
            }
            None => {
                out.push_str(&rest[s..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 未替换的占位符（渲染后检查，避免把 `{{foo}}` 原样发给 k8s）
pub fn unresolved(rendered: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = rendered.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 2] == b"{{" {
            if let Some(end) = rendered[i + 2..].find("}}") {
                out.push(rendered[i + 2..i + 2 + end].to_string());
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

pub async fn list(State(st): State<Arc<AppState>>, _u: AuthUser) -> ApiResult<Json<Value>> {
    let mut items = st.store.list_deploy_templates().await?;
    // 内置模板总是存在（库里没有就临时给出，不写库）
    if !items
        .iter()
        .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("builtin-sql-app"))
    {
        items.insert(
            0,
            json!({
                "id": "builtin-sql-app", "name": "SQL 作为 application（内置）",
                "kind": "sql-app", "builtin": true, "yamlTemplate": BUILTIN_SQL_APP,
            }),
        );
    }
    if !items
        .iter()
        .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("builtin-sql-sessionjob"))
    {
        items.insert(
            1,
            json!({
                "id": "builtin-sql-sessionjob", "name": "SQL 作为 SessionJob（内置）",
                "kind": "sql-sessionjob", "builtin": true, "yamlTemplate": BUILTIN_SQL_SESSIONJOB,
            }),
        );
    }
    let placeholders = [
        "name",
        "namespace",
        "image",
        "flinkVersion",
        "serviceAccount",
        "slots",
        "restartStrategy",
        "checkpointDir",
        "savepointDir",
        "jmCpu",
        "jmMemory",
        "tmCpu",
        "tmMemory",
        "tmReplicas",
        "runnerJar",
        "args",
        "parallelism",
        "upgradeMode",
        "sqlConfigMap",
        "sessionCluster",
    ];
    Ok(Json(
        json!({ "items": items, "placeholders": placeholders }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBody {
    pub id: Option<String>,
    pub name: String,
    pub kind: Option<String>,
    pub yaml_template: String,
}

pub async fn save(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<SaveBody>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    if body.yaml_template.trim().is_empty() {
        return Err(ApiError(Error::BadRequest("yamlTemplate 不能为空".into())));
    }
    let id = body.id.clone().unwrap_or_else(|| {
        body.name
            .trim()
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric(), "-")
    });
    if id.starts_with("builtin-") {
        return Err(ApiError(Error::BadRequest(
            "内置模板不能覆盖，请另存为新 id".into(),
        )));
    }
    // 语法体检：先把所有 {{x}} 换成一个裸标识符再解析。
    // 否则 `replicas: {{tmReplicas}}` 会被 YAML 当成 map 而误报。
    let probe = strip_placeholders(&body.yaml_template);
    serde_yaml::from_str::<Value>(&probe)
        .map_err(|e| ApiError(Error::BadRequest(format!("模板不是合法 YAML：{e}"))))?;

    let kind = body.kind.unwrap_or_else(|| "sql-app".into());
    st.store
        .save_deploy_template(&id, body.name.trim(), &kind, &body.yaml_template)
        .await?;
    st.audit(&user, "deploy-template.save", &id, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub async fn remove(
    State(st): State<Arc<AppState>>,
    u: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let AuthUser { user, .. } = u;

    st.store.delete_deploy_template(&id).await?;
    st.audit(
        &user,
        "deploy-template.delete",
        &id,
        "",
        "",
        "ok",
        None,
        None,
    );
    Ok(Json(json!({ "ok": true })))
}

/// 取一个模板（内置或自定义），供 deploy 流程使用
pub async fn resolve_template(st: &Arc<AppState>, id: Option<&str>) -> Result<String, Error> {
    let wanted = id.unwrap_or("builtin-sql-app");
    if wanted == "builtin-sql-app" {
        return Ok(BUILTIN_SQL_APP.to_string());
    }
    if wanted == "builtin-sql-sessionjob" {
        return Ok(BUILTIN_SQL_SESSIONJOB.to_string());
    }
    match st.store.get_deploy_template(wanted).await? {
        Some(t) => Ok(t
            .get("yamlTemplate")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()),
        None => Err(Error::NotFound(format!("部署模板 {wanted} 不存在"))),
    }
}
