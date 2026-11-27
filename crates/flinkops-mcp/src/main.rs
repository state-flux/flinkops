//! FlinkOps MCP server（stdio + Streamable HTTP）。
//!
//! 设计要点（详见 `docs/mcp-spec.md`）：
//! - **没有集群凭据**：所有动作都翻译成平台 REST 调用，平台上再走 scope/范围/审批/审计
//! - **默认最小权限**：Token 由网页创建，`allow_ns` / `allow_jobs` 决定能碰哪些作业
//! - **高危操作要人点头**：写工具首次调用返回 `approval_required`，审批后用 approvalToken 重试
//!
//! 两种传输：
//! ```text
//! # stdio（Claude Desktop / Cursor 的默认方式）
//! FLINKOPS_URL=http://127.0.0.1:8080 FLINKOPS_MCP_TOKEN=fko_pat_x_y flinkops-mcp --stdio
//!
//! # Streamable HTTP（远程/多客户端，POST /mcp 收发 JSON-RPC）
//! FLINKOPS_URL=http://127.0.0.1:8080 FLINKOPS_MCP_TOKEN=fko_pat_x_y flinkops-mcp --http 127.0.0.1:8090
//! ```
//! 排障：`--check` 只验证 Token；`--list-tools` 打印工具清单。

mod client;
mod redact;
mod tools;

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use client::Platform;

const PROTOCOL_VERSION: &str = "2024-11-05";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let base = std::env::var("FLINKOPS_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let token = std::env::var("FLINKOPS_MCP_TOKEN").unwrap_or_default();
    let default_approval = std::env::var("FLINKOPS_MCP_APPROVAL").ok();

    if args.iter().any(|a| a == "--list-tools") {
        for t in tools::specs() {
            println!("{}\t{}", t.name, t.description);
        }
        return;
    }
    if token.is_empty() {
        eprintln!("缺少 FLINKOPS_MCP_TOKEN（在 网页 → MCP 与审批 里创建）");
        std::process::exit(2);
    }
    let platform = Platform::new(&base, &token);

    if args.iter().any(|a| a == "--check") {
        match platform.get("/api/me").await {
            Ok(v) => println!(
                "ok: {} @ {}",
                v.get("user").and_then(|u| u.as_str()).unwrap_or("?"),
                base
            ),
            Err(e) => {
                eprintln!("token 不可用：{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(addr) = http_addr(&args) {
        serve_http(platform, default_approval, &addr).await;
        return;
    }

    eprintln!("flinkops-mcp: stdio ready → {base}");
    run_stdio(platform, default_approval).await;
}

fn http_addr(args: &[String]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if a == "--http" {
            return args
                .get(i + 1)
                .cloned()
                .or_else(|| Some("127.0.0.1:8090".into()));
        }
        if let Some(rest) = a.strip_prefix("--http=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// 一条 JSON-RPC 消息 → 一条响应（通知返回 None）。
async fn handle(platform: &Platform, default_approval: Option<&str>, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let is_notification = req.get("id").is_none();

    let resp = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "flinkops-mcp", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "FlinkOps 的运维工具面：读状态、看指标、触发 savepoint；挂起/扩缩容/恢复/换镜像/删除需要人工审批（首次调用返回 approval_required，请在网页审批后用 approvalToken 重试）。"
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools::specs().iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
            })).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let mut arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            if let (Some(tok), Some(map)) = (default_approval, arguments.as_object_mut()) {
                if !map.contains_key("approvalToken") {
                    map.insert("approvalToken".into(), Value::String(tok.to_string()));
                }
            }
            call_tool(platform, &name, &arguments).await
        }
        "notifications/initialized" | "initialized" => Ok(Value::Null),
        other => Err(anyhow::anyhow!("method not found: {other}")),
    };

    if is_notification {
        return None;
    }
    Some(match resp {
        Ok(v) if v.is_null() => return None,
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Err(e) => err_resp(id, -32603, &e.to_string()),
    })
}

async fn call_tool(platform: &Platform, name: &str, args: &Value) -> Result<Value> {
    match tools::call(platform, name, args).await {
        Ok(v) => Ok(json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()) }],
            "isError": false
        })),
        Err(e) => Ok(json!({
            "content": [{ "type": "text", "text": format!("{e}") }],
            "isError": true
        })),
    }
}

fn err_resp(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn run_stdio(platform: Platform, default_approval: Option<String>) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    out,
                    "{}",
                    err_resp(Value::Null, -32700, &format!("parse error: {e}"))
                );
                let _ = out.flush();
                continue;
            }
        };
        if let Some(resp) = handle(&platform, default_approval.as_deref(), &req).await {
            let _ = writeln!(out, "{resp}");
            let _ = out.flush();
        }
    }
}

/// Streamable HTTP：`POST /mcp` 收一条（或一批）JSON-RPC，返回 JSON。
///
/// 不做 SSE 流式：工具调用是"一问一答"，纯 JSON 已足够，任何 HTTP 客户端都能接。
async fn serve_http(platform: Platform, default_approval: Option<String>, addr: &str) {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    #[derive(Clone)]
    struct Ctx {
        platform: Platform,
        approval: Option<String>,
    }

    async fn mcp(State(ctx): State<Ctx>, body: String) -> axum::response::Response {
        let parsed: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(err_resp(Value::Null, -32700, &format!("parse error: {e}"))),
                )
                    .into_response()
            }
        };
        // 支持 batch（数组）：逐条处理，丢掉通知
        if let Some(batch) = parsed.as_array() {
            let mut out = Vec::new();
            for item in batch {
                if let Some(r) = handle(&ctx.platform, ctx.approval.as_deref(), item).await {
                    out.push(r);
                }
            }
            if out.is_empty() {
                return StatusCode::ACCEPTED.into_response();
            }
            return Json(Value::Array(out)).into_response();
        }
        match handle(&ctx.platform, ctx.approval.as_deref(), &parsed).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        }
    }

    async fn info() -> impl IntoResponse {
        Json(json!({
            "name": "flinkops-mcp",
            "transport": "streamable-http",
            "endpoint": "POST /mcp",
            "protocolVersion": PROTOCOL_VERSION,
            "hint": "用 MCP 客户端把 url 指向 http://<host>/mcp；healthz 在 /healthz"
        }))
    }

    async fn healthz() -> impl IntoResponse {
        (StatusCode::OK, "ok")
    }

    let ctx = Ctx {
        platform,
        approval: default_approval,
    };
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route("/", get(info))
        .route("/healthz", get(healthz))
        .with_state(ctx);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {addr} 失败：{e}");
            std::process::exit(1);
        }
    };
    eprintln!("flinkops-mcp: http ready → http://{addr}/mcp");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("http server 退出：{e}");
    }
}
