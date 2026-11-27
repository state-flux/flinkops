//! 路由级集成测试（真正走 axum 中间件栈：rbac / csrf / 限流 / 审计）。
//!
//! 不依赖集群：AppState 的 clusters 为空，测的是"认证与治理"这条链路。
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use flinkops_api::{routes, AppState, Config};
use flinkops_store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn make_app() -> (axum::Router, Arc<AppState>) {
    // 纯 Rust TLS provider：测试里构造 reqwest::Client 之前必须装（rustls-no-provider）
    flinkops_tls::install();
    let path = std::env::temp_dir().join(format!("flinkops-test-{}.db", uuid::Uuid::new_v4()));
    // Windows 路径要转成正斜杠，且 sqlite 需要三斜杠（sqlite:///C:/...）
    let dsn = format!(
        "sqlite:///{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let store = Store::connect(&dsn).await.expect("connect sqlite");
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
    (routes::build(state.clone()), state)
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

fn json_req(
    method: &str,
    uri: &str,
    body: &str,
    cookies: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookies {
        b = b.header(header::COOKIE, c);
    }
    if let Some(t) = csrf {
        b = b.header("x-csrf-token", t);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// 从 Set-Cookie 头里取出 `name=value`（多个 Set-Cookie 时逐个找）。
fn pick_cookie(resp: &axum::response::Response, name: &str) -> Option<String> {
    for v in resp.headers().get_all(header::SET_COOKIE) {
        let s = v.to_str().ok()?;
        if let Some(rest) = s.strip_prefix(&format!("{name}=")) {
            return Some(rest.split(';').next()?.to_string());
        }
    }
    None
}

#[tokio::test]
async fn unauthenticated_api_returns_json_401() {
    let (app, _st) = make_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("application/json"));
}

#[tokio::test]
async fn login_sets_session_and_csrf_then_write_requires_csrf() {
    let (app, _st) = make_app().await;
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"admin","password":"admin"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let session = pick_cookie(&resp, "flinkops_session").expect("session cookie");
    let csrf = pick_cookie(&resp, "flinkops_csrf").expect("csrf cookie");
    let cookies = format!("flinkops_session={session}; flinkops_csrf={csrf}");

    // 读：只要会话
    let resp = app
        .clone()
        .oneshot(json_req("GET", "/api/jobs", "", Some(&cookies), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 写：缺 CSRF → 403
    let resp = app
        .clone()
        .oneshot(json_req("POST", "/api/jobs/sync", "", Some(&cookies), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_text(resp).await;
    assert!(body.contains("\"csrf\""), "unexpected body: {body}");

    // 写：带 CSRF → 200
    let resp = app
        .oneshot(json_req(
            "POST",
            "/api/jobs/sync",
            "",
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_failures_are_rate_limited() {
    let (app, _st) = make_app().await;
    let mut last = StatusCode::OK;
    for _ in 0..5 {
        let resp = app
            .clone()
            .oneshot(json_req(
                "POST",
                "/api/login",
                r#"{"username":"admin","password":"wrong"}"#,
                None,
                None,
            ))
            .await
            .unwrap();
        last = resp.status();
    }
    // 第 5 次失败触发锁定，但响应本身仍是 401；第 6 次才 429
    assert_eq!(last, StatusCode::UNAUTHORIZED, "第 5 次仍是 401");
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"admin","password":"wrong"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "第 6 次应被锁定"
    );

    // 锁定期内即使密码正确也拒绝
    let resp = app
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"admin","password":"admin"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().get(header::RETRY_AFTER).is_some());
}

#[tokio::test]
async fn local_user_login_uses_argon2_and_password_change_works() {
    let (app, st) = make_app().await;
    st.store
        .upsert_user(
            "alice",
            Some(&flinkops_store::hash_password("alice-pass-1")),
            "viewer",
            &[],
        )
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"alice","password":"alice-pass-1"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let session = pick_cookie(&resp, "flinkops_session").unwrap();
    let csrf = pick_cookie(&resp, "flinkops_csrf").unwrap();
    let cookies = format!("flinkops_session={session}; flinkops_csrf={csrf}");

    // viewer 是只读角色：写操作应 403（rbac）
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/jobs/sync",
            "",
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 改密码：旧密码错 → 400；正确 → 200，并能用新密码登录
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/me/password",
            r#"{"oldPassword":"nope","newPassword":"alice-pass-2"}"#,
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/me/password",
            r#"{"oldPassword":"alice-pass-1","newPassword":"alice-pass-2"}"#,
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"alice","password":"alice-pass-2"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn audit_can_be_filtered_and_exported_as_csv() {
    let (app, st) = make_app().await;
    // 直接写库（AppState::audit 是 fire-and-forget，测试里会读到空表）
    for (actor, action) in [("alice", "job.restart"), ("bob", "job.delete")] {
        let entry = flinkops_types::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            ts: flinkops_types::now_rfc3339(),
            actor: actor.into(),
            action: action.into(),
            target: "orders".into(),
            cluster: "local".into(),
            namespace: "flink".into(),
            result: "ok".into(),
            detail: None,
            ip: None,
        };
        st.store.audit(&entry).await.unwrap();
    }

    // 登录拿会话（audit 只对 admin 开放）
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"admin","password":"admin"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    let session = pick_cookie(&resp, "flinkops_session").unwrap();
    let cookies = format!("flinkops_session={session}");

    let resp = app
        .clone()
        .oneshot(json_req(
            "GET",
            "/api/audit?action=job.restart",
            "",
            Some(&cookies),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_text(resp).await;
    assert!(body.contains("job.restart"));
    assert!(!body.contains("job.delete"), "过滤没生效：{body}");

    let resp = app
        .oneshot(json_req(
            "GET",
            "/api/audit?format=csv&action=job.restart",
            "",
            Some(&cookies),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/csv"));
    let csv = body_text(resp).await;
    assert!(csv.starts_with('\u{feff}'), "CSV 应带 BOM 便于 Excel 识别");
    assert!(csv.contains("ts,actor,action"));
    assert!(csv.contains("job.restart"));
}

/// 接入集群的"测试连通性"与"粘贴 kubeconfig"都必须先校验内容：
/// 垃圾内容要在落盘/建连接前就被拒，且不能留下集群条目。
#[tokio::test]
async fn cluster_endpoints_reject_broken_kubeconfig() {
    let (app, _st) = make_app().await;
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/login",
            r#"{"username":"admin","password":"admin"}"#,
            None,
            None,
        ))
        .await
        .unwrap();
    let session = pick_cookie(&resp, "flinkops_session").unwrap();
    let csrf = pick_cookie(&resp, "flinkops_csrf").unwrap();
    let cookies = format!("flinkops_session={session}; flinkops_csrf={csrf}");

    // 1) 测试连通性：垃圾 YAML → 400（不连网）
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/clusters/test",
            r#"{"kubeconfig":"this is not a kubeconfig"}"#,
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "垃圾 kubeconfig 应 400"
    );
    let body = body_text(resp).await;
    assert!(
        body.contains("kubeconfig"),
        "错误应指出是 kubeconfig：{body}"
    );

    // 2) 接入：同样内容不能创建出集群
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/clusters",
            r#"{"id":"broken","kubeconfig":"this is not a kubeconfig"}"#,
            Some(&cookies),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .oneshot(json_req("GET", "/api/clusters", "", Some(&cookies), None))
        .await
        .unwrap();
    let body = body_text(resp).await;
    assert!(!body.contains("\"broken\""), "失败不应留下集群条目：{body}");
}
