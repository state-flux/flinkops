//! 工具面（见 `docs/mcp-spec.md §3`）。
//!
//! 三档：只读（read）/ 生命周期（operate）/ 高危（admin）。
//! 这里只做参数校验与"翻译成平台 REST 调用"；**真正的授权、范围、审批、审计在平台侧**，
//! 所以即使有人绕过这个二进制直接打 REST，破坏力一样被闸门卡住。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::client::Platform;
use crate::redact::redacted_yaml_payload;

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

fn obj_schema(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

pub fn specs() -> Vec<ToolSpec> {
    let job = || {
        json!({
            "cluster": s("集群 id，例如 local"),
            "namespace": s("命名空间，例如 flink"),
            "name": s("FlinkDeployment 名称")
        })
    };
    let mut v = Vec::new();

    v.push(ToolSpec {
        name: "flinkops_list_jobs",
        description: "列出 FlinkDeployment。可按集群/命名空间/状态/健康度过滤。返回 summary 数组。",
        schema: obj_schema(
            json!({
                "cluster": s("可选：集群 id"),
                "namespace": s("可选：命名空间"),
                "state": s("可选：RUNNING / FAILED / SUSPENDED …"),
                "health": s("可选：green / yellow / red"),
                "limit": json!({ "type": "integer", "minimum": 1, "maximum": 50 })
            }),
            &[],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_get_job",
        description: "取单个作业的详情：summary + checkpoint 统计 + savepoint 历史。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_job_metrics",
        description:
            "作业实时指标（走 JobManager REST）。返回 degraded 字段时说明上游降级，不要当 0 解读。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_job_exceptions",
        description: "作业根因异常（最近几条）。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_job_events",
        description: "作业相关 K8s 事件（最近 N 条）。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "limit": json!({ "type": "integer", "minimum": 1, "maximum": 100 })
            }),
            &["cluster", "namespace", "name"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_job_yaml",
        description: "作业 CRD YAML（已脱敏：去掉 podTemplate / last-applied / managedFields，env 值打码）。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_list_alerts",
        description: "告警事件列表（默认全部，可只取 OPEN / ACK）。",
        schema: obj_schema(
            json!({
                "state": s("可选：OPEN / ACK"),
                "severity": s("可选：danger / warn / info")
            }),
            &[],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_list_snapshots",
        description: "FlinkStateSnapshot 列表（savepoint 的 CR 视图）。",
        schema: obj_schema(
            json!({
                "cluster": s("可选"), "namespace": s("可选"), "state": s("可选：COMPLETED / FAILED / IN_PROGRESS")
            }),
            &[],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_prom_query",
        description: "PromQL 瞬时查询（只允许 flink_/kube_/node_/container_/machine_/process_ 前缀的指标）。",
        schema: obj_schema(json!({ "query": s("PromQL 表达式") }), &["query"]),
    });

    v.push(ToolSpec {
        name: "flinkops_savepoint",
        description:
            "触发一次 savepoint（不改运行状态，无需审批）。返回 mode=cr 表示快照 CR 已创建。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "path": s("可选：savepoint 目录；留空用作业配置的 state.savepoints.dir")
            }),
            &["cluster", "namespace", "name"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_restart",
        description: "原地重启（重建 JobManager，不改状态）。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_suspend",
        description: "挂起作业（会停掉 JobManager/TaskManager）。**需要人工审批**：首次调用返回 approval_required，拿到审批后用 approvalToken 重试。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "approvalToken": s("可选：网页审批中心批准后拿到的一次性 token")
            }),
            &["cluster", "namespace", "name"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_resume",
        description: "启动作业（挂起后恢复；会先清理 operator 残留的 -rest Service）。",
        schema: obj_schema(job(), &["cluster", "namespace", "name"]),
    });
    v.push(ToolSpec {
        name: "flinkops_scale",
        description: "调并行度 / TaskManager 副本。**需要人工审批**。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "parallelism": json!({ "type": "integer", "minimum": 1 }),
                "taskManagerReplicas": json!({ "type": "integer", "minimum": 1 }),
                "approvalToken": s("审批 token")
            }),
            &["cluster", "namespace", "name"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_restore",
        description:
            "从指定 savepoint 恢复（必须显式给路径，不接受'最近一次'）。**需要人工审批**。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "savepointPath": s("savepoint 的完整路径（hdfs:// 或 s3://）"),
                "approvalToken": s("审批 token")
            }),
            &["cluster", "namespace", "name", "savepointPath"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_upgrade_image",
        description: "滚动升级镜像（默认强制 upgradeMode=savepoint）。**需要人工审批**。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "image": s("新镜像"),
                "upgradeMode": s("可选：savepoint / last-state / stateless，默认 savepoint"),
                "approvalToken": s("审批 token")
            }),
            &["cluster", "namespace", "name", "image"],
        ),
    });
    v.push(ToolSpec {
        name: "flinkops_delete_job",
        description:
            "删除作业（不可恢复）。**admin scope + 审批 + confirmName 必须与作业名完全一致**。",
        schema: obj_schema(
            json!({
                "cluster": s("集群 id"), "namespace": s("命名空间"), "name": s("作业名"),
                "confirmName": s("再次输入作业名以确认"),
                "approvalToken": s("审批 token")
            }),
            &["cluster", "namespace", "name", "confirmName"],
        ),
    });

    v
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn need(args: &Value, key: &str) -> Result<String> {
    arg_str(args, key).ok_or_else(|| anyhow!("缺少参数 {key}"))
}

fn job_path(args: &Value, suffix: &str) -> Result<String> {
    let cluster = need(args, "cluster")?;
    let ns = need(args, "namespace")?;
    let name = need(args, "name")?;
    Ok(format!("/api/jobs/{cluster}/{ns}/{name}{suffix}"))
}

fn approval(args: &Value) -> Option<String> {
    arg_str(args, "approvalToken")
}

pub async fn call(p: &Platform, name: &str, args: &Value) -> Result<Value> {
    match name {
        "flinkops_list_jobs" => {
            let mut q = Vec::new();
            for k in ["cluster", "namespace", "state", "health"] {
                if let Some(v) = arg_str(args, k) {
                    q.push(format!("{k}={}", urlencode(&v)));
                }
            }
            let path = if q.is_empty() {
                "/api/jobs".to_string()
            } else {
                format!("/api/jobs?{}", q.join("&"))
            };
            let out = p.get(&path).await?;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .clamp(1, 50) as usize;
            let items = out
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let truncated = items.len() > limit;
            let items: Vec<Value> = items.into_iter().take(limit).collect();
            Ok(json!({ "items": items, "count": items.len(), "truncated": truncated }))
        }
        "flinkops_get_job" => {
            let out = p.get(&job_path(args, "")?).await?;
            Ok(json!({
                "summary": out.get("summary").cloned().unwrap_or(Value::Null),
                "checkpoint": out.get("checkpoint").cloned().unwrap_or(Value::Null),
                "savepoint": out.get("savepoint").cloned().unwrap_or(Value::Null),
            }))
        }
        "flinkops_job_metrics" => p.get(&job_path(args, "/metrics")?).await,
        "flinkops_job_exceptions" => p.get(&job_path(args, "/exceptions")?).await,
        "flinkops_job_events" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .clamp(1, 100);
            p.get(&job_path(args, &format!("/events?limit={limit}"))?)
                .await
        }
        "flinkops_job_yaml" => {
            let out = p.get(&job_path(args, "/yaml")?).await?;
            let text = out
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| out.to_string());
            Ok(redacted_yaml_payload(&text))
        }
        "flinkops_list_alerts" => {
            let mut q = Vec::new();
            if let Some(v) = arg_str(args, "state") {
                q.push(format!("state={}", urlencode(&v)));
            }
            let path = if q.is_empty() {
                "/api/alerts/events".to_string()
            } else {
                format!("/api/alerts/events?{}", q.join("&"))
            };
            let out = p.get(&path).await?;
            let severity = arg_str(args, "severity");
            let items: Vec<Value> = out
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| {
                    severity
                        .as_deref()
                        .map(|s| e.get("severity").and_then(|x| x.as_str()) == Some(s))
                        .unwrap_or(true)
                })
                .take(50)
                .collect();
            Ok(json!({ "items": items, "open": out.get("open").cloned().unwrap_or(Value::Null) }))
        }
        "flinkops_list_snapshots" => {
            let mut q = Vec::new();
            for k in ["cluster", "namespace", "state"] {
                if let Some(v) = arg_str(args, k) {
                    q.push(format!("{k}={}", urlencode(&v)));
                }
            }
            let path = if q.is_empty() {
                "/api/snapshots".to_string()
            } else {
                format!("/api/snapshots?{}", q.join("&"))
            };
            p.get(&path).await
        }
        "flinkops_prom_query" => {
            let query = need(args, "query")?;
            p.get(&format!("/api/metrics/query?query={}", urlencode(&query)))
                .await
        }
        "flinkops_savepoint" => {
            let body = json!({ "path": arg_str(args, "path") });
            p.post(&job_path(args, "/savepoint")?, body, None).await
        }
        "flinkops_restart" => p.post(&job_path(args, "/restart")?, json!({}), None).await,
        "flinkops_suspend" => {
            p.post(
                &job_path(args, "/state")?,
                json!({ "running": false }),
                approval(args).as_deref(),
            )
            .await
        }
        "flinkops_resume" => {
            p.post(&job_path(args, "/state")?, json!({ "running": true }), None)
                .await
        }
        "flinkops_scale" => {
            let mut body = serde_json::Map::new();
            if let Some(v) = args.get("parallelism") {
                body.insert("parallelism".into(), v.clone());
            }
            if let Some(v) = args.get("taskManagerReplicas") {
                body.insert("taskManagerReplicas".into(), v.clone());
            }
            if body.is_empty() {
                return Err(anyhow!("parallelism 或 taskManagerReplicas 至少给一个"));
            }
            p.post(
                &job_path(args, "/scale")?,
                Value::Object(body),
                approval(args).as_deref(),
            )
            .await
        }
        "flinkops_restore" => {
            let path = need(args, "savepointPath")?;
            p.post(
                &job_path(args, "/restore")?,
                json!({ "path": path }),
                approval(args).as_deref(),
            )
            .await
        }
        "flinkops_upgrade_image" => {
            let image = need(args, "image")?;
            let mode = arg_str(args, "upgradeMode").unwrap_or_else(|| "savepoint".into());
            p.post(
                &job_path(args, "/image")?,
                json!({ "image": image, "upgradeMode": mode }),
                approval(args).as_deref(),
            )
            .await
        }
        "flinkops_delete_job" => {
            let name_arg = arg_str(args, "name").unwrap_or_default();
            let confirm = arg_str(args, "confirmName").unwrap_or_default();
            if confirm != name_arg {
                return Err(anyhow!("confirmName 与 name 不一致，拒绝执行"));
            }
            p.delete(&job_path(args, "")?, approval(args).as_deref())
                .await
        }
        other => Err(anyhow!("未知工具 {other}")),
    }
}

/// 极简 YAML → JSON：只用于脱敏，解析失败回退为纯文本展示。
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
