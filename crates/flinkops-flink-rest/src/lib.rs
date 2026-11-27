//! Flink JobManager REST 客户端（WebUI 背后的同一套接口）。
//!
//! 原则：**单作业能查到的实时数据一律走这里**，Prometheus 只补历史。
//! 两个硬约束必须遵守，否则会拖慢生产作业：
//! 1. metrics 必须用 `?get=a,b,c` 批量，禁止空 `get`（会扫全量）；
//! 2. 单个 JobManager 是单线程 async，调用方要限并发（默认 4/作业）+ 2s 超时；
//! 3. 背压端点每次请求会采样 100 次任务线程，30–60s 一次即可。
use std::collections::BTreeMap;
use std::time::Duration;

use flinkops_types::{Error, Result};
use serde::Serialize;
use serde_json::Value;

/// 常用指标名（一次批量取，别分开请求）。
/// Job 级指标：只放 Flink 1.15 → 1.20 都存在的名字。
///
/// 注意：`numRecordsInPerSecond` 这类吞吐指标**不在 job 级**（1.15 的
/// `/jobs/:id/metrics` 里没有），必须按 vertex 聚合，见 `job_throughput`。
pub const LIVE_METRICS: &[&str] = &[
    "numberOfCompletedCheckpoints",
    "numberOfFailedCheckpoints",
    "lastCheckpointDuration",
    "lastCheckpointSize",
    "fullRestarts",
    "uptime",
];

/// 从 metrics 响应里取值。兼容两种线上格式：
/// - Flink ≤1.15 及部分版本：`[{"id":"x","value":"1"}]`
/// - 新版本：`{"id":"jobid","metrics":[{"id":"x","value":"1"}]}`
fn parse_metrics(v: &Value) -> BTreeMap<String, f64> {
    let empty: &[Value] = &[];
    let items: &[Value] = match v {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) => v
            .get("metrics")
            .and_then(|m| m.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(empty),
        _ => empty,
    };
    let mut out = BTreeMap::new();
    for item in items {
        let Some(id) = item.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(raw) = item.get("value").and_then(|x| x.as_str()) else {
            continue;
        };
        if let Ok(n) = raw.parse::<f64>() {
            out.insert(id.to_string(), n);
        }
    }
    out
}

/// 累计吞吐（records in / out，按 vertex 求和）。
///
/// `rate_*` 是该顶点 subtask 级 `*PerSecond` 的聚合和：Flink 1.15+ 在 vertex 级
/// `/metrics` 里取不到 `numRecordsIn/Out`（返回 `[]`），只有 subtask 聚合接口稳定可用。
#[derive(Debug, Clone, Copy, Default)]
pub struct Throughput {
    pub records_in: f64,
    pub records_out: f64,
    pub rate_in: f64,
    pub rate_out: f64,
    pub vertices: usize,
}

/// 解析 subtask 聚合格式：`[{"id":"x","min":..,"max":..,"avg":..,"sum":..}]`。
fn parse_agg(v: &Value) -> BTreeMap<String, f64> {
    let empty: &[Value] = &[];
    let items: &[Value] = match v {
        Value::Array(a) => a.as_slice(),
        _ => empty,
    };
    let mut out = BTreeMap::new();
    for item in items {
        let Some(id) = item.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let n = item
            .get("sum")
            .and_then(|x| x.as_f64())
            .or_else(|| item.get("avg").and_then(|x| x.as_f64()));
        if let Some(n) = n {
            out.insert(id.to_string(), n);
        }
    }
    out
}

#[derive(Clone)]
pub struct FlinkRest {
    http: reqwest::Client,
    base: String,
}

#[derive(Debug, Clone, Copy)]
pub enum LogTarget {
    JobManagerMain,
    JobManagerStdout,
    TaskManager(&'static str),
}

impl FlinkRest {
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .connect_timeout(Duration::from_millis(500))
            .pool_max_idle_per_host(8)
            .build()
            .unwrap_or_else(|e| {
                // 不能 unwrap_or_default：那会得到一个"没有超时"的 client，
                // 后端卡住时请求会一直挂着。
                tracing::error!(error = %e, "build reqwest client failed, falling back with 5s timeout");
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .expect("plain client")
            });
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    /// 用调用方提供的 client（共享连接池，避免每个请求都重新握手）。
    pub fn with_client(base: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    /// 平台在集群内时，直接用 Operator 生成的 rest Service DNS。
    pub fn for_job(namespace: &str, name: &str) -> Self {
        Self::new(format!(
            "http://{name}-rest.{namespace}.svc.cluster.local:8081"
        ))
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn get_json(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: url.clone(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream {
                target: url,
                message: format!("{status}: {}", body.chars().take(300).collect::<String>()),
            });
        }
        resp.json::<Value>().await.map_err(|e| Error::Upstream {
            target: url,
            message: format!("decode json: {e}"),
        })
    }

    pub async fn get_text(&self, path: &str) -> Result<String> {
        self.get_text_limited(path, 2 * 1024 * 1024).await
    }

    /// 带字节上限的文本读取：日志文件可能是几百 MB，全量进内存会打爆进程。
    /// 超限时保留**尾部**（排障最关心最近的异常）并加截断标记。
    pub async fn get_text_limited(&self, path: &str, max_bytes: usize) -> Result<String> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: url.clone(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Upstream {
                target: url,
                message: status.to_string(),
            });
        }
        let bytes = resp.bytes().await.map_err(|e| Error::Upstream {
            target: url.clone(),
            message: e.to_string(),
        })?;
        if bytes.len() <= max_bytes {
            return Ok(String::from_utf8_lossy(&bytes).to_string());
        }
        let tail = &bytes[bytes.len() - max_bytes..];
        Ok(format!(
            "[已截断] 日志共 {} 字节，仅显示最后 {} KB\n{}",
            bytes.len(),
            max_bytes / 1024,
            String::from_utf8_lossy(tail)
        ))
    }

    pub async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: url.clone(),
                message: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Upstream {
                target: url,
                message: status.to_string(),
            });
        }
        resp.json::<Value>().await.map_err(|e| Error::Upstream {
            target: url,
            message: format!("decode json: {e}"),
        })
    }

    // ------------------------------------------------------------ jobs

    pub async fn overview(&self) -> Result<Value> {
        self.get_json("/overview").await
    }

    pub async fn job(&self, job_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}")).await
    }

    pub async fn jobs_overview(&self) -> Result<Value> {
        self.get_json("/jobs/overview").await
    }

    pub async fn job_config(&self, job_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}/config")).await
    }

    pub async fn exceptions(&self, job_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}/exceptions")).await
    }

    /// 批量取指标。**必须传非空 ids**。
    pub async fn metrics(&self, job_id: &str, ids: &[&str]) -> Result<BTreeMap<String, f64>> {
        if ids.is_empty() {
            return Err(Error::BadRequest(
                "metrics: ids 不能为空（避免全量扫描）".into(),
            ));
        }
        let joined = ids.join(",");
        let v = self
            .get_json(&format!("/jobs/{job_id}/metrics?get={joined}"))
            .await?;
        Ok(parse_metrics(&v))
    }

    /// 直接触发 savepoint（不走 FlinkStateSnapshot CR）。
    /// 返回 Flink 的 request-id，可用 `/jobs/:id/savepoints/:triggerId` 查进度。
    pub async fn trigger_savepoint(&self, job_id: &str, target_dir: Option<&str>) -> Result<Value> {
        let mut body = serde_json::json!({ "cancel-job": false });
        if let Some(dir) = target_dir {
            body["target-dir"] = serde_json::json!(dir);
        }
        self.post_json(&format!("/jobs/{job_id}/savepoints"), body)
            .await
    }

    /// 取消作业（`PATCH /jobs/:id?mode=cancel`）。
    pub async fn cancel_job(&self, job_id: &str) -> Result<()> {
        let url = format!("{}/jobs/{}?mode=cancel", self.base, job_id);
        let resp = self
            .http
            .patch(&url)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: "flink-rest".into(),
                message: format!("{url}: {e}"),
            })?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream {
                target: "flink-rest".into(),
                message: format!(
                    "cancel -> {code}: {}",
                    body.chars().take(200).collect::<String>()
                ),
            });
        }
        Ok(())
    }

    /// 轮询 savepoint 直到完成，返回最终路径（失败返回 None）。
    pub async fn wait_savepoint(
        &self,
        job_id: &str,
        trigger_id: &str,
        timeout_secs: u64,
    ) -> Result<Option<String>> {
        let url = format!("{}/jobs/{}/savepoints/{}", self.base, job_id, trigger_id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            let resp = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| Error::Upstream {
                    target: "flink-rest".into(),
                    message: format!("{url}: {e}"),
                })?;
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            let status = v
                .get("status")
                .and_then(|x| x.get("id"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            match status {
                "COMPLETED" => {
                    return Ok(v
                        .get("operation")
                        .and_then(|o| o.get("location"))
                        .and_then(|x| x.as_str())
                        .map(String::from))
                }
                "FAILED" => return Ok(None),
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                return Ok(None);
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// checkpoint 统计（counts + 最近一次）。历史明细不在这里。
    pub async fn checkpoints(&self, job_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}/checkpoints")).await
    }

    /// 顶点级背压（会采样 100 次任务线程，别高频调用）。
    pub async fn backpressure(&self, job_id: &str, vertex_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}/vertices/{vertex_id}/backpressure"))
            .await
    }

    pub async fn vertices(&self, job_id: &str) -> Result<Value> {
        self.get_json(&format!("/jobs/{job_id}")).await
    }

    /// vertex 级指标（吞吐、busy、背压都在这一层）。
    pub async fn vertex_metrics(
        &self,
        job_id: &str,
        vertex_id: &str,
        ids: &[&str],
    ) -> Result<BTreeMap<String, f64>> {
        if ids.is_empty() {
            return Err(Error::BadRequest("vertex metrics: ids 不能为空".into()));
        }
        let joined = ids.join(",");
        let v = self
            .get_json(&format!(
                "/jobs/{job_id}/vertices/{vertex_id}/metrics?get={joined}"
            ))
            .await?;
        Ok(parse_metrics(&v))
    }

    /// 顶点 × subtask 聚合指标（`/vertices/:id/subtasks/metrics`，返回 `sum`）。
    ///
    /// 为什么不直接用 `vertex_metrics`：Flink 1.15 的 vertex 级 `/metrics` 对
    /// `numRecordsIn/numRecordsOut/*PerSecond` 一律返回 `[]`（实测），subtask 聚合
    /// 接口才是稳的。作业顶点通常 1–6 个，串行请求可接受。
    pub async fn vertex_subtask_metrics(
        &self,
        job_id: &str,
        vertex_id: &str,
        ids: &[&str],
    ) -> Result<BTreeMap<String, f64>> {
        if ids.is_empty() {
            return Err(Error::BadRequest("subtask metrics: ids 不能为空".into()));
        }
        let joined = ids.join(",");
        let v = self
            .get_json(&format!(
                "/jobs/{job_id}/vertices/{vertex_id}/subtasks/metrics?get={joined}"
            ))
            .await?;
        Ok(parse_agg(&v))
    }

    /// Job 的累计 + 实时吞吐：取 `/jobs/:id` 的 vertices，再逐 vertex 求 subtask 聚合之和。
    ///
    /// 语义与 Flink UI 一致（job 级没有吞吐指标）：跨顶点求和，链式算子会重复计数，
    /// 只用于"有没有在动"的判断，不做计费口径。
    pub async fn job_throughput(&self, job_id: &str) -> Result<Throughput> {
        let job = self.job(job_id).await?;
        let verts = job
            .get("vertices")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut out = Throughput::default();
        for v in verts {
            let Some(vid) = v.get("id").and_then(|x| x.as_str()) else {
                continue;
            };
            match self
                .vertex_subtask_metrics(
                    job_id,
                    vid,
                    &[
                        "numRecordsIn",
                        "numRecordsOut",
                        "numRecordsInPerSecond",
                        "numRecordsOutPerSecond",
                    ],
                )
                .await
            {
                Ok(m) => {
                    out.records_in += m.get("numRecordsIn").copied().unwrap_or(0.0);
                    out.records_out += m.get("numRecordsOut").copied().unwrap_or(0.0);
                    out.rate_in += m.get("numRecordsInPerSecond").copied().unwrap_or(0.0);
                    out.rate_out += m.get("numRecordsOutPerSecond").copied().unwrap_or(0.0);
                    out.vertices += 1;
                }
                // 单个 vertex 取不到不影响整体
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    pub async fn taskmanagers(&self) -> Result<Value> {
        self.get_json("/taskmanagers").await
    }

    // ------------------------------------------------------------ logs

    /// 注意：K8s 上默认只输出 stdout，`/jobmanager/log` 需要 log4j 配了
    /// file appender 才有内容；生产优先用 Pod logs（kubectl logs / Loki）。
    pub async fn log(&self, target: LogTarget) -> Result<String> {
        let path = match target {
            LogTarget::JobManagerMain => "/jobmanager/log".to_string(),
            LogTarget::JobManagerStdout => "/jobmanager/stdout".to_string(),
            LogTarget::TaskManager(id) => format!("/taskmanagers/{id}/stdout"),
        };
        self.get_text(&path).await
    }

    // ------------------------------------------------------------ profiler

    /// FLIP-375（Flink 1.18+）：async-profiler 全进程采样。
    pub async fn profiler_jobmanager(&self, duration_secs: u32) -> Result<Value> {
        self.post_json(
            "/jobmanager/profiler",
            serde_json::json!({ "type": "full", "duration": duration_secs }),
        )
        .await
    }

    pub async fn profiler_taskmanager(&self, tm_id: &str, duration_secs: u32) -> Result<Value> {
        self.post_json(
            &format!("/taskmanagers/{tm_id}/profiler"),
            serde_json::json!({ "type": "full", "duration": duration_secs }),
        )
        .await
    }

    /// 顶点级火焰图（Flink 1.13+，轻量线程栈采样）。
    pub async fn vertex_flamegraph(
        &self,
        job_id: &str,
        vertex_id: &str,
        kind: &str,
    ) -> Result<Value> {
        self.get_json(&format!(
            "/jobs/{job_id}/vertices/{vertex_id}/flamegraph?type={kind}"
        ))
        .await
    }
}

// ---------------------------------------------------------------- SQL Gateway

/// Flink SQL Gateway（1.16+ 的独立进程，默认 8083）。
///
/// 注意：它和 JobManager REST 不是一个东西，base 也不同。
#[derive(Clone)]
pub struct SqlGateway {
    base: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub took_ms: u64,
    /// INSERT 类语句提交后 Flink 返回的 jobId
    pub job_id: Option<String>,
}

impl SqlGateway {
    pub fn new(base: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: "sql-gateway".into(),
                message: format!("{url}: {e}"),
            })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Upstream {
                target: "sql-gateway".into(),
                message: format!(
                    "{path} -> {status}: {}",
                    text.chars().take(300).collect::<String>()
                ),
            });
        }
        serde_json::from_str(&text)
            .map_err(|e| Error::Internal(format!("sql-gateway 解码失败: {e}")))
    }

    /// 开一个 SQL 会话，返回 session handle。
    pub async fn open_session(&self) -> Result<String> {
        let v = self
            .post("/v1/sessions", serde_json::json!({ "properties": {} }))
            .await?;
        v.get("sessionHandle")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| Error::Internal("sql-gateway: 响应里没有 sessionHandle".into()))
    }

    /// 提交一条语句，返回 operation handle。
    pub async fn execute(&self, session: &str, sql: &str) -> Result<String> {
        let v = self
            .post(
                &format!("/v1/sessions/{session}/statements"),
                serde_json::json!({ "statement": sql }),
            )
            .await?;
        v.get("operationHandle")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| Error::Internal("sql-gateway: 响应里没有 operationHandle".into()))
    }

    /// 取结果：gateway 是分页的，要跟随 nextResultUri 累积到 max_rows。
    pub async fn fetch(&self, session: &str, op: &str, max_rows: usize) -> Result<SqlResult> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut uri = format!(
            "{}/v1/sessions/{session}/operations/{op}/result/0?rowFormat=JSON",
            self.base
        );
        let mut columns: Vec<String> = Vec::new();
        let mut data: Vec<serde_json::Value> = Vec::new();

        loop {
            let resp = self
                .http
                .get(&uri)
                .send()
                .await
                .map_err(|e| Error::Upstream {
                    target: "sql-gateway".into(),
                    message: format!("{uri}: {e}"),
                })?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();

            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_e) => {
                    return Err(Error::Internal(format!(
                        "sql-gateway 返回非 JSON（{status}）: {}",
                        text.chars().take(200).collect::<String>()
                    )))
                }
            };

            let rt = v.get("resultType").and_then(|x| x.as_str()).unwrap_or("");

            // 还在算
            if status.as_u16() == 202 || rt == "NOT_READY" || rt == "PAYLOAD_NOT_READY" {
                if std::time::Instant::now() > deadline {
                    return Err(Error::Upstream {
                        target: "sql-gateway".into(),
                        message: "结果 90s 未就绪".into(),
                    });
                }
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Upstream {
                    target: "sql-gateway".into(),
                    message: format!(
                        "result -> {status}: {}",
                        text.chars().take(300).collect::<String>()
                    ),
                });
            }

            if columns.is_empty() {
                if let Some(arr) = v.pointer("/results/columns").and_then(|c| c.as_array()) {
                    columns = arr
                        .iter()
                        .map(|c| {
                            c.get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("col")
                                .to_string()
                        })
                        .collect();
                }
            }
            if let Some(arr) = v.pointer("/results/data").and_then(|d| d.as_array()) {
                data.extend(arr.iter().cloned());
            }

            if data.len() >= max_rows {
                break;
            }
            match v.get("nextResultUri").and_then(|x| x.as_str()) {
                Some(next) if !next.is_empty() => {
                    uri = if next.starts_with("http") {
                        next.to_string()
                    } else {
                        format!("{}{}?rowFormat=JSON", self.base, next)
                    };
                }
                _ => break,
            }
        }

        let truncated = data.len() > max_rows;
        // INSERT 提交后 gateway 返回 {"kind":"INSERT","fields":["<32 位 hex jobId>"]}，
        // 这个 jobId 是后续 suspend / 打快照的凭据，别当普通数据行丢掉。
        let job_id = data.iter().find_map(|row| {
            let m = row.as_object()?;
            if m.get("kind").and_then(|k| k.as_str()) != Some("INSERT") {
                return None;
            }
            let v = m.get("fields")?.as_array()?.first()?.as_str()?;
            if v.len() == 32 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                Some(v.to_string())
            } else {
                None
            }
        });
        let rows: Vec<Vec<serde_json::Value>> = data
            .into_iter()
            .take(max_rows)
            .map(|row| {
                let obj = match &row {
                    serde_json::Value::Object(m) => Some(m),
                    _ => None,
                };
                if let Some(m) = obj {
                    // collect 结果形态：{"kind":"INSERT","fields":["1","2"]}
                    if let Some(fields) = m.get("fields").and_then(|f| f.as_array()) {
                        return fields.clone();
                    }
                    if !columns.is_empty() {
                        return columns
                            .iter()
                            .map(|c| m.get(c).cloned().unwrap_or(serde_json::Value::Null))
                            .collect();
                    }
                    return m.values().cloned().collect();
                }
                match row {
                    serde_json::Value::Array(a) => a,
                    other => vec![other],
                }
            })
            .collect();

        Ok(SqlResult {
            row_count: rows.len(),
            columns,
            rows,
            truncated,
            took_ms: 0,
            job_id,
        })
    }
    pub async fn close_session(&self, session: &str) -> Result<()> {
        let url = format!("{}/v1/sessions/{session}", self.base);
        let _ = self.http.delete(&url).send().await;
        Ok(())
    }
}
// ---------------------------------------------------------------- Catalog 浏览
impl SqlGateway {
    async fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Upstream {
                target: "sql-gateway".into(),
                message: format!("{url}: {e}"),
            })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Upstream {
                target: "sql-gateway".into(),
                message: format!(
                    "{path} -> {status}: {}",
                    text.chars().take(300).collect::<String>()
                ),
            });
        }
        serde_json::from_str(&text).map_err(|e| Error::Internal(format!("解码失败: {e}")))
    }

    fn pick_names(v: &serde_json::Value, key: &str) -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// GET /v1/catalogs
    pub async fn catalogs(&self) -> Result<Vec<String>> {
        let v = self.get_json("/v1/catalogs").await?;
        Ok(Self::pick_names(&v, "catalogs"))
    }

    /// GET /v1/sessions/{h}/catalogs/{c}/databases
    pub async fn databases(&self, session: &str, catalog: &str) -> Result<Vec<String>> {
        let path = format!("/v1/sessions/{session}/catalogs/{catalog}/databases");
        let v = self.get_json(&path).await?;
        Ok(Self::pick_names(&v, "databases"))
    }

    /// GET /v1/sessions/{h}/catalogs/{c}/databases/{db}/tables
    pub async fn tables(&self, session: &str, catalog: &str, db: &str) -> Result<Vec<String>> {
        let path = format!("/v1/sessions/{session}/catalogs/{catalog}/databases/{db}/tables");
        let v = self.get_json(&path).await?;
        Ok(Self::pick_names(&v, "tables"))
    }

    /// GET /v1/sessions/{h}/catalogs/{c}/databases/{db}/tables/{t}
    pub async fn table_schema(
        &self,
        session: &str,
        catalog: &str,
        db: &str,
        table: &str,
    ) -> Result<serde_json::Value> {
        let path =
            format!("/v1/sessions/{session}/catalogs/{catalog}/databases/{db}/tables/{table}");
        self.get_json(&path).await
    }
}
