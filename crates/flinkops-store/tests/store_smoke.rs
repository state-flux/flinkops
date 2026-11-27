//! 非 SQLite 后端的端到端冒烟（默认跳过）。
//!
//! 覆盖三件最容易在换库时翻车的事：
//! 1. **DDL 方言**：MySQL 的 `TEXT` 不能做主键、不能带 DEFAULT；
//! 2. **占位符**：Postgres 需要 `$1`，MySQL/SQLite 用 `?`；
//! 3. **upsert 语义**：MySQL 对"值没变"的 UPDATE 返回 0 行，若据此走 INSERT 会撞主键。
//!
//! 运行：
//! ```bash
//! FLINKOPS_TEST_DSN='postgres://postgres:pg@127.0.0.1:5432/flinkops' \
//!   cargo test -p flinkops-store --test store_smoke -- --ignored --nocapture
//! FLINKOPS_TEST_DSN='mysql://u:p@127.0.0.1:3306/flinkops' \
//!   cargo test -p flinkops-store --test store_smoke -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use flinkops_store::Store;
use flinkops_types::{AuditEntry, Profile};

fn dsn() -> Option<String> {
    std::env::var("FLINKOPS_TEST_DSN")
        .or_else(|_| std::env::var("FLINKOPS_TEST_PG"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL/MySQL：设置 FLINKOPS_TEST_DSN 后加 --ignored 运行"]
async fn non_sqlite_roundtrip() {
    let Some(dsn) = dsn() else {
        eprintln!("跳过：未设置 FLINKOPS_TEST_DSN");
        return;
    };
    let backend = if dsn.starts_with("postgres") {
        "postgres"
    } else if dsn.starts_with("mysql") {
        "mysql"
    } else {
        "sqlite"
    };

    let store = Store::connect(&dsn).await.expect("connect");
    assert_eq!(store.dialect().as_str(), backend, "方言应与 DSN 一致");

    // 换 DDL 时旧表结构会挡路：FLINKOPS_TEST_RESET=1 先清表再迁移
    if std::env::var("FLINKOPS_TEST_RESET").is_ok() {
        for tbl in [
            "flinkops_jobs",
            "flinkops_audit",
            "flinkops_profiles",
            "flinkops_sessions",
            "flinkops_users",
            "flinkops_clusters",
            "flinkops_settings",
            "flinkops_alert_rules",
            "flinkops_alert_events",
            "flinkops_mcp_tokens",
            "flinkops_mcp_approvals",
            "flinkops_catalogs",
            "flinkops_catalog_tables",
            "flinkops_sql_jobs",
            "flinkops_sql_versions",
            "flinkops_templates",
            "flinkops_deploy_templates",
            "flinkops_schema_migrations",
        ] {
            let _ = sqlx::query(&format!("DROP TABLE IF EXISTS {tbl}"))
                .execute(store.pool())
                .await;
        }
        println!("已清表（FLINKOPS_TEST_RESET）");
    }
    store.migrate().await.expect("migrate");
    // 迁移必须幂等：再跑一次不能报错
    store.migrate().await.expect("migrate again (idempotent)");

    // ---- 1) 写入 + 带条件查询（占位符改写 / LIMIT/OFFSET / LIKE） ----
    let uniq = format!("smoke-{}", uuid::Uuid::new_v4());
    let entry = AuditEntry {
        id: uniq.clone(),
        ts: flinkops_types::now_rfc3339(),
        actor: "smoke".into(),
        action: "test.store".into(),
        target: "smoke".into(),
        cluster: "c".into(),
        namespace: "n".into(),
        result: "ok".into(),
        detail: None,
        ip: None,
    };
    store.audit(&entry).await.expect("insert audit");
    let (items, total) = store
        .list_audit_filtered(&flinkops_store::AuditFilter {
            actor: Some("smoke".into()),
            keyword: Some("smoke".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("query audit");
    assert!(total >= 1, "总数应 >= 1");
    assert!(items.iter().any(|i| i.id == uniq), "刚写入的审计应能查回");

    // ---- 2) upsert 语义：同一份数据写两次（MySQL 的 0-rows 陷阱） ----
    let mut kv = HashMap::new();
    kv.insert("smoke.key".to_string(), "v1".to_string());
    store.set_settings(&kv).await.expect("set settings #1");
    store
        .set_settings(&kv)
        .await
        .expect("set settings #2（值未变）");
    // 用 get_settings 而不是 get_setting：后者把错误吞成 None，排查时看不到原因
    let settings = store.get_settings().await.expect("get settings");
    assert_eq!(
        settings.get("smoke.key").map(String::as_str),
        Some("v1"),
        "重复写入后值应保持（实际 settings = {settings:?}）"
    );

    let profile = Profile {
        id: "smoke-profile".into(),
        name: "smoke".into(),
        flink_version: "v1_17".into(),
        image: "repo/app:1.0".into(),
        namespace: "flink".into(),
        checkpoint_prefix: "s3://b/cp".into(),
        savepoint_prefix: "s3://b/sp".into(),
        state_backend: "rocksdb".into(),
        rest_exposed: "ClusterIP".into(),
        metrics_port: 9250,
        service_account: "flink".into(),
        defaults: Default::default(),
        builtin: false,
        labels: Default::default(),
    };
    store.save_profile(&profile).await.expect("save profile #1");
    store
        .save_profile(&profile)
        .await
        .expect("save profile #2（值未变）");
    let list = store.list_profiles().await.expect("list profiles");
    assert!(
        list.iter().any(|p| p.id == "smoke-profile"),
        "profile 应存在且不重复"
    );

    // ---- 3) 整数列解码（PG 的 INT4 不能读成 i64；DDL 统一 BIGINT） ----
    let jobs = store.list_jobs(None).await.expect("list jobs");
    println!("{backend} 冒烟通过：migrate×2 / audit / settings upsert / profile upsert / list jobs（{} 条）", jobs.len());
}
