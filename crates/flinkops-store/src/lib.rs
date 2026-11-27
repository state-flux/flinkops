//! 元数据存储：作业索引、审计、环境基线。
//!
//! 后端用 `sqlx::Any`：同一套代码支持
//! `sqlite:///data/flinkops.db`、`postgres://…`、`mysql://…`。
//! 因此 SQL 只用三种方言的交集（TEXT / BIGINT），不用 PG 分区表，
//! 保留期靠按时间 `DELETE` + 定期 `VACUUM`（SQLite）。
use std::time::Duration;

use anyhow::Context;
use flinkops_types::{AuditEntry, Error, JobSummary, Profile, Result};
use sqlx::{AnyPool, Row};

use dialect::Dialect;

mod dialect;
pub mod users;

mod alerts;
mod audit;
mod catalogs;
mod jobs;
mod mcp;
mod platform;
mod profiles;
mod sessions;
pub use users::{hash_password, needs_rehash, verify_password, UserRecord};

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS flinkops_jobs (
        cluster TEXT NOT NULL,
        ns TEXT NOT NULL,
        name TEXT NOT NULL,
        kind TEXT,
        state TEXT,
        job_id TEXT,
        parallelism BIGINT,
        tm_replicas BIGINT,
        slots_per_tm BIGINT,
        restart_nonce BIGINT,
        upgrade_mode TEXT,
        image TEXT,
        jm_cpu TEXT,
        jm_mem TEXT,
        tm_cpu TEXT,
        tm_mem TEXT,
        flink_version TEXT,
        rest_exposed TEXT,
        jm_status TEXT,
        recon_state TEXT,
        err_text TEXT,
        generation BIGINT,
        observed_generation BIGINT,
        conditions TEXT,
        lifecycle_state TEXT,
        started_at TEXT,
        updated_at TEXT,
        synced_at TEXT,
        PRIMARY KEY (cluster, ns, name)
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_audit (
        id TEXT PRIMARY KEY,
        ts TEXT NOT NULL,
        actor TEXT,
        action TEXT,
        target TEXT,
        cluster TEXT,
        ns TEXT,
        result TEXT,
        detail TEXT,
        ip TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_deploy_templates (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        yaml_template TEXT NOT NULL,
        builtin BIGINT,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_catalogs (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        dsn TEXT NOT NULL,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_catalog_tables (
        id TEXT PRIMARY KEY,
        catalog_id TEXT NOT NULL,
        db_name TEXT,
        table_name TEXT,
        columns_json TEXT,
        synced_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_sql_jobs (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        cluster TEXT,
        ns TEXT,
        session TEXT,
        sql_text TEXT NOT NULL,
        version BIGINT,
        job_id TEXT,
        state TEXT,
        last_error TEXT,
        created_at TEXT,
        updated_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_sql_versions (
        id TEXT PRIMARY KEY,
        sql_job_id TEXT NOT NULL,
        version BIGINT,
        sql_text TEXT NOT NULL,
        note TEXT,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_templates (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT,
        source_job TEXT,
        payload TEXT NOT NULL,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_clusters (
        id TEXT PRIMARY KEY,
        source_kind TEXT NOT NULL,
        kubeconfig_path TEXT,
        readonly BIGINT,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_settings (
        k TEXT PRIMARY KEY,
        v TEXT NOT NULL,
        updated_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_alert_rules (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        threshold BIGINT,
        severity TEXT,
        enabled BIGINT,
        created_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_alert_events (
        id TEXT PRIMARY KEY,
        rule_id TEXT,
        rule_name TEXT,
        kind TEXT,
        cluster TEXT,
        ns TEXT,
        job TEXT,
        severity TEXT,
        message TEXT,
        state TEXT,
        ts TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_sessions (
        token TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        created_at TEXT NOT NULL,
        expires_at TEXT NOT NULL
    )",
    // 多租户账号：role = admin | operator | viewer；allow_ns 为该用户的命名空间白名单
    // （admin 忽略；operator/viewer 为空则除了公共只读外什么都看不到）
    "CREATE TABLE IF NOT EXISTS flinkops_users (
        username TEXT PRIMARY KEY,
        password_hash TEXT NOT NULL,
        role TEXT NOT NULL,
        allow_ns TEXT NOT NULL DEFAULT '[]',
        disabled BIGINT NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        totp_secret TEXT,
        totp_pending TEXT
    )",
    "CREATE TABLE IF NOT EXISTS flinkops_profiles (
        id TEXT PRIMARY KEY,
        name TEXT,
        payload TEXT,
        builtin BIGINT,
        updated_at TEXT
    )",
    // MCP：机器认证用的 API Token（只存 sha256(secret)）
    "CREATE TABLE IF NOT EXISTS flinkops_mcp_tokens (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        secret_hash TEXT NOT NULL,
        scopes TEXT NOT NULL,
        allow_ns TEXT NOT NULL,
        allow_jobs TEXT NOT NULL,
        qps BIGINT,
        expires_at TEXT,
        created_at TEXT NOT NULL,
        last_used_at TEXT,
        revoked_at TEXT
    )",
    // MCP：写操作的一次性审批
    "CREATE TABLE IF NOT EXISTS flinkops_mcp_approvals (
        id TEXT PRIMARY KEY,
        tool TEXT NOT NULL,
        args_hash TEXT NOT NULL,
        target TEXT NOT NULL,
        actor TEXT NOT NULL,
        token_id TEXT NOT NULL,
        status TEXT NOT NULL,
        token_hash TEXT,
        created_at TEXT NOT NULL,
        decided_at TEXT,
        decided_by TEXT
    )",
];

/// 迁移步骤：版本号必须单调递增，且**一旦发布就不能改**（老库按版本号跳过）。
fn schema_steps() -> Vec<(i64, &'static [&'static str])> {
    const V2: &[&str] = &[
        "ALTER TABLE flinkops_jobs ADD COLUMN lifecycle_state TEXT",
        "ALTER TABLE flinkops_jobs ADD COLUMN started_at TEXT",
        "ALTER TABLE flinkops_jobs ADD COLUMN jm_cpu TEXT",
        "ALTER TABLE flinkops_jobs ADD COLUMN jm_mem TEXT",
        "ALTER TABLE flinkops_jobs ADD COLUMN tm_cpu TEXT",
        "ALTER TABLE flinkops_jobs ADD COLUMN tm_mem TEXT",
    ];
    // v3：账号表补 TOTP（新库在 v1 的 CREATE TABLE 里已有，这里给老库补）
    const V3: &[&str] = &[
        "ALTER TABLE flinkops_users ADD COLUMN totp_secret TEXT",
        "ALTER TABLE flinkops_users ADD COLUMN totp_pending TEXT",
    ];
    vec![(1, MIGRATIONS), (2, V2), (3, V3)]
}

/// `ADD COLUMN` 重复执行（新库 v1 已建好列）不算失败。
fn is_idempotent_schema_error(e: &sqlx::Error) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("duplicate column")
        || msg.contains("already exists")
        || msg.contains("duplicate field")
}

/// 审计查询条件（拆分出来是为了让 `list_audit_filtered` 的参数不至于爆表）。
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub action: Option<String>,
    pub actor: Option<String>,
    pub result: Option<String>,
    pub namespace: Option<String>,
    pub keyword: Option<String>,
    /// RFC3339 下界（ISO 字典序可比较）
    pub since: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone)]
pub struct Store {
    pool: AnyPool,
    /// 方言决定占位符写法（PG 用 $1）与哪些维护操作可用
    dialect: Dialect,
    /// 原始 DSN，用于判断 sqlite 文件大小
    dsn_hint: Option<String>,
}

/// MCP API Token（不含 secret；secret 只存 sha256）。
#[derive(Debug, Clone)]
pub struct McpToken {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub allow_ns: Vec<String>,
    pub allow_jobs: Vec<String>,
    pub qps: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// MCP 写操作的一次性审批。
#[derive(Debug, Clone)]
pub struct McpApproval {
    pub id: String,
    pub tool: String,
    pub args_hash: String,
    pub target: String,
    pub actor: String,
    pub token_id: String,
    pub status: String,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
}

impl Store {
    pub async fn connect(dsn: &str) -> anyhow::Result<Self> {
        sqlx::any::install_default_drivers();
        let dialect = Dialect::from_dsn(dsn)?;
        if dialect.is_sqlite() {
            ensure_sqlite_parent(dsn)?;
        }
        // SQLite 是单写者，连接数给小一点；PG 允许并发
        let max_conns = if dialect.is_sqlite() { 4 } else { 10 };
        let pool = sqlx::any::AnyPoolOptions::new()
            .max_connections(max_conns)
            .acquire_timeout(Duration::from_secs(5))
            .connect(dsn)
            .await
            .with_context(|| format!("connect {} database", dialect.as_str()))?;
        Ok(Self {
            pool,
            dialect,
            dsn_hint: Some(dsn.to_string()),
        })
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    /// 当前方言（health / doctor / 日志用）
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// 发送前按方言改写占位符：SQLite/MySQL 原样返回，Postgres 把 `?` 变成 `$1`。
    /// 所有 SQL 都必须经过这里，否则 Postgres 会在 Parse 阶段报错。
    fn sql<'a>(&self, sql: &'a str) -> std::borrow::Cow<'a, str> {
        self.dialect.placeholders(sql)
    }

    /// 版本化迁移：每个版本在一个事务里跑完并记录到 `flinkops_schema_migrations`。
    ///
    /// 之前的实现是"能跑就跑、失败就忽略"，升级出问题时没有任何痕迹，
    /// 也没法知道库停在哪个版本。现在：
    /// - 版本号单调递增，已应用的直接跳过（幂等、可重复启动）；
    /// - 同版本内的语句在事务里执行，**真的失败就报错**，不会带着半套 schema 起服务；
    /// - 只有"列已存在"（`ADD COLUMN` 重复执行）被显式容忍 —— 这是历史上
    ///   "CREATE TABLE IF NOT EXISTS + ALTER" 混用留下的兼容点。
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(&self.sql(
            "CREATE TABLE IF NOT EXISTS flinkops_schema_migrations (
                version BIGINT PRIMARY KEY,
                applied_at TEXT NOT NULL
            )",
        ))
        .execute(&self.pool)
        .await?;
        let applied: Vec<i64> =
            sqlx::query_scalar(&self.sql("SELECT version FROM flinkops_schema_migrations"))
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();

        for (version, stmts) in schema_steps() {
            if applied.contains(&version) {
                continue;
            }
            let mut tx = self
                .pool
                .begin()
                .await
                .context("begin migration transaction")?;
            for stmt in stmts {
                let ddl = self.dialect.ddl(stmt);
                let stmt_sql = self.sql(&ddl);
                // Postgres 里"列已存在"会让整个事务进入 aborted，光 catch 错误不够，
                // 必须 ROLLBACK TO SAVEPOINT 才能继续；SQLite 同理支持 SAVEPOINT。
                // MySQL 的 SAVEPOINT 走不了 prepared statement（1295），而且 DDL 自己
                // 隐式提交、不会污染后续语句，所以那边直接执行、容忍"已存在"。
                let use_savepoint = self.dialect != Dialect::MySql;
                if use_savepoint {
                    sqlx::query("SAVEPOINT flinkops_mig")
                        .execute(&mut *tx)
                        .await?;
                }
                match sqlx::query(&stmt_sql).execute(&mut *tx).await {
                    Ok(_) => {
                        if use_savepoint {
                            sqlx::query("RELEASE SAVEPOINT flinkops_mig")
                                .execute(&mut *tx)
                                .await?;
                        }
                    }
                    Err(e) if is_idempotent_schema_error(&e) => {
                        if use_savepoint {
                            sqlx::query("ROLLBACK TO SAVEPOINT flinkops_mig")
                                .execute(&mut *tx)
                                .await?;
                            sqlx::query("RELEASE SAVEPOINT flinkops_mig")
                                .execute(&mut *tx)
                                .await?;
                        }
                        tracing::debug!(version, error = %e, "列/对象已存在，跳过");
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "schema migration v{version} 失败：{e}（语句：{}）",
                            stmt.trim().lines().next().unwrap_or("")
                        ));
                    }
                }
            }
            sqlx::query(
                &self.sql(
                    "INSERT INTO flinkops_schema_migrations (version, applied_at) VALUES (?, ?)",
                ),
            )
            .bind(version)
            .bind(flinkops_types::now_rfc3339())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(version, "schema migration applied");
        }
        Ok(())
    }

    // ---------------------------------------------------------- jobs
}

/// 从 DSN 里解析出 sqlite 文件路径（仅用于展示占用，不参与连接）。
fn sqlite_path(dsn: &str) -> Option<&str> {
    let rest = dsn.strip_prefix("sqlite://")?;
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" || path == "/:memory:" {
        return None;
    }
    #[cfg(windows)]
    let path = path.strip_prefix('/').unwrap_or(path);
    Some(path)
}

fn ensure_sqlite_parent(dsn: &str) -> anyhow::Result<()> {
    if let Some(path) = sqlite_path(dsn) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
    }
    Ok(())
}
/// 读字符串列：先按 `String` 解，失败再按 `Vec<u8>` 解后转 UTF-8。
///
/// 为什么需要：`sqlx::Any` 把 MySQL 的 TEXT/MEDIUMTEXT 报告成 `BLOB`，
/// `String` 会直接解码失败（错误：mismatched types ... SQL type BLOB）。
/// 长文本列（SQL / YAML / JSON）在三种库上统一走这个入口。
pub fn text_col(r: &sqlx::any::AnyRow, col: &str) -> String {
    use sqlx::Row;
    if let Ok(s) = r.try_get::<String, _>(col) {
        return s;
    }
    if let Ok(b) = r.try_get::<Vec<u8>, _>(col) {
        return String::from_utf8_lossy(&b).into_owned();
    }
    String::new()
}

/// 同上，但空值返回 None。
pub fn text_col_opt(r: &sqlx::any::AnyRow, col: &str) -> Option<String> {
    use sqlx::Row;
    if let Ok(Some(v)) = r.try_get::<Option<String>, _>(col) {
        return Some(v);
    }
    if let Ok(Some(b)) = r.try_get::<Option<Vec<u8>>, _>(col) {
        return Some(String::from_utf8_lossy(&b).into_owned());
    }
    None
}

fn sql_job_row(r: sqlx::any::AnyRow) -> serde_json::Value {
    use sqlx::Row;
    serde_json::json!({
        "id": r.try_get::<String, _>("id").unwrap_or_default(),
        "name": r.try_get::<String, _>("name").unwrap_or_default(),
        "cluster": r.try_get::<Option<String>, _>("cluster").ok().flatten(),
        "namespace": r.try_get::<Option<String>, _>("ns").ok().flatten(),
        "session": r.try_get::<Option<String>, _>("session").ok().flatten(),
        "sqlText": text_col(&r, "sql_text"),
        "version": r.try_get::<i64, _>("version").unwrap_or(1),
        "jobId": r.try_get::<Option<String>, _>("job_id").ok().flatten(),
        "state": r.try_get::<Option<String>, _>("state").ok().flatten(),
        "lastError": r.try_get::<Option<String>, _>("last_error").ok().flatten(),
        "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
        "updatedAt": r.try_get::<String, _>("updated_at").unwrap_or_default(),
    })
}
/// DSN 脱敏：mysql://user:pass@host:port/db → mysql://user:***@host:port/db
fn mask_dsn(dsn: &str) -> String {
    if let Some(at) = dsn.rfind('@') {
        if let Some(colon) = dsn[..at].rfind(':') {
            let head = &dsn[..colon];
            let tail = &dsn[at..];
            return format!("{head}:***{tail}");
        }
    }
    dsn.to_string()
}

/// 在给定连接/事务上执行 upsert（UPDATE 命中 0 行则 INSERT）。
///
/// 抽出来是为了让"全量同步"能跑在同一个事务里（`replace_cluster_jobs`），
/// 单条写入（写路径刷新索引）复用同一段 SQL，避免两处漂移。
async fn upsert_job_on(
    conn: &mut sqlx::AnyConnection,
    dialect: Dialect,
    j: &JobSummary,
) -> Result<()> {
    let now = flinkops_types::now_rfc3339();
    let conditions = serde_json::to_string(&j.conditions).unwrap_or_else(|_| "[]".into());

    let updated = sqlx::query(&dialect.placeholders(
        "UPDATE flinkops_jobs SET kind=?, state=?, job_id=?, parallelism=?, tm_replicas=?,
         slots_per_tm=?, restart_nonce=?, upgrade_mode=?, image=?, flink_version=?, rest_exposed=?,
         jm_status=?, recon_state=?, err_text=?, generation=?, observed_generation=?,
         conditions=?, lifecycle_state=?, started_at=?, updated_at=?, synced_at=?
         WHERE cluster=? AND ns=? AND name=?",
    ))
    .bind(&j.kind)
    .bind(&j.state)
    .bind(&j.job_id)
    .bind(j.parallelism)
    .bind(j.task_manager_replicas)
    .bind(j.slots_per_tm)
    .bind(j.restart_nonce)
    .bind(&j.upgrade_mode)
    .bind(&j.image)
    .bind(&j.flink_version)
    .bind(&j.rest_exposed)
    .bind(&j.job_manager_status)
    .bind(&j.reconciliation_state)
    .bind(&j.error)
    .bind(j.generation)
    .bind(j.observed_generation)
    .bind(conditions)
    .bind(&j.lifecycle_state)
    .bind(&j.started_at)
    .bind(&j.updated_at)
    .bind(&now)
    .bind(&j.cluster)
    .bind(&j.namespace)
    .bind(&j.name)
    .execute(&mut *conn)
    .await
    .map_err(|e| Error::Internal(format!("upsert job: {e}")))?;

    if updated.rows_affected() == 0 {
        sqlx::query(&dialect.placeholders(
            "INSERT INTO flinkops_jobs (cluster, ns, name, kind, state, job_id, parallelism,
             tm_replicas, slots_per_tm, restart_nonce, upgrade_mode, image, jm_cpu, jm_mem,
             tm_cpu, tm_mem, flink_version, rest_exposed, jm_status, recon_state, err_text,
             generation, observed_generation, conditions, lifecycle_state, started_at,
             updated_at, synced_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        ))
        .bind(&j.cluster)
        .bind(&j.namespace)
        .bind(&j.name)
        .bind(&j.kind)
        .bind(&j.state)
        .bind(&j.job_id)
        .bind(j.parallelism)
        .bind(j.task_manager_replicas)
        .bind(j.slots_per_tm)
        .bind(j.restart_nonce)
        .bind(&j.upgrade_mode)
        .bind(&j.image)
        .bind(&j.job_manager_cpu)
        .bind(&j.job_manager_memory)
        .bind(&j.task_manager_cpu)
        .bind(&j.task_manager_memory)
        .bind(&j.flink_version)
        .bind(&j.rest_exposed)
        .bind(&j.job_manager_status)
        .bind(&j.reconciliation_state)
        .bind(&j.error)
        .bind(j.generation)
        .bind(j.observed_generation)
        .bind(serde_json::to_string(&j.conditions).unwrap_or_else(|_| "[]".into()))
        .bind(&j.lifecycle_state)
        .bind(&j.started_at)
        .bind(&j.updated_at)
        .bind(&now)
        .execute(&mut *conn)
        .await
        .map_err(|e| Error::Internal(format!("insert job: {e}")))?;
    }
    Ok(())
}
