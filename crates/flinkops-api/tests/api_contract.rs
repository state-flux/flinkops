//! 契约测试：`docs/api-spec.md` 里写明的 GET 端点，路由表必须真的有。
//!
//! 为什么需要它：文档与实现脱节是这类项目最常见的腐化方式 —— 改路由忘了改
//! spec，或者 spec 里的端点早被删了还能骗过 review。这里直接把 spec 当输入，
//! 对每个 GET 路径发一次请求：只要不是 404，就说明路由存在（401/400/403 都正常）。
//!
//! 范围说明：`api-spec.md` 目前显式写了 14 个端点（其余在表格/正文里描述），
//! 所以这条测试守的是"写下来的必须存在"，不是"全部端点都被写下来"。
//! 后续把整套端点补进 spec，这个测试会自动变严格。
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use flinkops_api::{routes, AppState, Config};
use flinkops_store::Store;
use tower::ServiceExt;

const SPEC: &str = include_str!("../../../docs/api-spec.md");

/// 从 spec 的总表里抽出 (method, path)：`| POST | \`/api/jobs\` |`，
/// 标题式 `### GET /api/me` 也算。
fn documented_endpoints() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in SPEC.lines() {
        // 表格式
        if line.starts_with("| ") {
            let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
            if cols.len() >= 3 {
                let method = cols[1];
                let path = cols[2].trim_matches('`');
                if matches!(method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
                    && path.starts_with("/api/")
                {
                    out.push((method.to_string(), path.to_string()));
                    continue;
                }
                if method == "ANY" && path.starts_with("/api/") {
                    out.push(("GET".to_string(), path.to_string()));
                    continue;
                }
            }
        }
        // 标题式
        for m in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            let needle = format!("### {m} /api/");
            if let Some(pos) = line.find(&needle) {
                let rest = &line[pos + 4..];
                let raw = rest.split_whitespace().next().unwrap_or("");
                let path = raw.split('?').next().unwrap_or("").trim_end_matches('.');
                if path.starts_with("/api/") {
                    out.push((m.to_string(), path.to_string()));
                }
            }
        }
    }
    out.retain(|(_, p)| {
        p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '{' | '}'))
    });
    out.sort();
    out.dedup();
    out
}

fn concrete(path: &str) -> String {
    // {xxx} → x，让 axum 能匹配到 handler（值不重要，只需要"路由存在"）
    let mut out = String::new();
    let mut in_brace = false;
    for ch in path.chars() {
        match ch {
            '{' => {
                in_brace = true;
                out.push('x');
            }
            '}' => in_brace = false,
            _ if in_brace => {}
            c => out.push(c),
        }
    }
    out
}

async fn app() -> axum::Router {
    let path = std::env::temp_dir().join(format!("flinkops-contract-{}.db", uuid::Uuid::new_v4()));
    let dsn = format!(
        "sqlite:///{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let store = Store::connect(&dsn).await.expect("connect");
    store.migrate().await.expect("migrate");
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        store_dsn: dsn,
        web_dir: ".".into(),
        clusters: vec![],
        audit_retention_days: 90,
        db_max_mb: 512,
        prometheus_url: None,
        admin_user: "admin".into(),
        admin_password: "admin".into(),
        readonly: false,
        clusters_dir: std::env::temp_dir().join("flinkops-test-clusters"),
        cookie_secure: Some(false),
        open_browser: false,
    };
    let (approvals_tx, _rx) = tokio::sync::broadcast::channel(8);
    let state = Arc::new(AppState {
        cfg,
        store,
        clusters: tokio::sync::RwLock::new(HashMap::new()),
        cluster_sources: tokio::sync::RwLock::new(HashMap::new()),
        sql_sessions: tokio::sync::RwLock::new(HashMap::new()),
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        http: reqwest::Client::new(),
        rest_cache: tokio::sync::RwLock::new(HashMap::new()),
        prom_cache: tokio::sync::RwLock::new(None),
        throughput_cache: tokio::sync::RwLock::new(HashMap::new()),
        mcp_rate: tokio::sync::RwLock::new(HashMap::new()),
        approvals_tx,
        sync_lock: tokio::sync::Mutex::new(()),
        heavy_cache: tokio::sync::RwLock::new(HashMap::new()),
        actual_parallelism: std::sync::Mutex::new(HashMap::new()),
        login_throttle: std::sync::Mutex::new(HashMap::new()),
        last_index_at: std::sync::Mutex::new(None),
        inflight: std::sync::Mutex::new(HashMap::new()),
    });
    routes::build(state)
}

#[tokio::test]
async fn every_documented_endpoint_is_routed() {
    let eps = documented_endpoints();
    assert!(
        eps.len() >= 40,
        "spec 总表解析到 {} 条端点，明显偏少（格式变了？）",
        eps.len()
    );

    let router = app().await;
    let mut missing = Vec::new();
    for (method, path) in &eps {
        let uri = concrete(path);
        let req = Request::builder()
            .method(method.as_str())
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        if resp.status() == StatusCode::NOT_FOUND {
            missing.push(format!("{method} {uri}"));
        }
    }
    assert!(
        missing.is_empty(),
        "这些 spec 里的端点路由表里没有（或 handler 直接 404）：{missing:#?}"
    );
}
