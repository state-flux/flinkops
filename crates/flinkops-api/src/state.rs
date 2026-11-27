//! 运行时状态：配置、存储、集群句柄、会话、审计。
//!
//! 这里只放**跨 handler 共享**的东西；具体业务逻辑在 `routes/*`，
//! 领域逻辑在 `flinkops-core`，K8s / Flink REST 访问在各自 crate。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use flinkops_flink_rest::FlinkRest;
use flinkops_k8s::Cluster;
use flinkops_store::Store;
use flinkops_types::{now_rfc3339, AuditEntry, Error, Result};
use tokio::sync::RwLock;
use tracing::warn;

use crate::config::Config;

pub const SESSION_COOKIE: &str = "flinkops_session";
/// CSRF 双提交 Cookie（非 HttpOnly，前端读出来放进 `X-CSRF-Token` 头）
pub const CSRF_COOKIE: &str = "flinkops_csrf";
/// CSRF 请求头
pub const CSRF_HEADER: &str = "x-csrf-token";

/// 登录失败计数（按 IP+账号）：5 次 / 10 分钟窗口 → 锁 5 分钟。
pub const LOGIN_MAX_FAILS: u32 = 5;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(600);
pub const LOGIN_LOCK: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, Default)]
pub struct LoginThrottle {
    pub fails: u32,
    pub first_at: Option<Instant>,
    pub locked_until: Option<Instant>,
}

/// 会话主体：账号 + 角色 + 命名空间白名单。
///
/// 角色只有三档：admin（全权）/ operator（可写，限定白名单 ns）/ viewer（只读）。
#[derive(Clone, Debug)]
pub struct Principal {
    pub user: String,
    pub role: String,
    pub allow_ns: Vec<String>,
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
    pub fn can_write(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "operator")
    }
    /// admin 忽略白名单；operator/viewer 只允许白名单里的 namespace（支持 `*`）。
    pub fn ns_ok(&self, ns: &str) -> bool {
        self.is_admin() || self.allow_ns.iter().any(|p| p == "*" || p == ns)
    }
}

/// 会话有效期：与库里的 `flinkops_sessions.expires_at` 保持一致（24h）。
pub const SESSION_TTL: Duration = Duration::from_secs(24 * 3600);

pub struct AppState {
    pub cfg: Config,
    pub store: Store,
    pub clusters: RwLock<HashMap<String, Arc<Cluster>>>,
    /// 每个集群的接入来源（重连时要按原路重建 client）
    pub cluster_sources: RwLock<HashMap<String, crate::config::ClusterSource>>,
    /// SQL Gateway 会话复用：key = cluster/ns/svc → (sessionHandle, 创建时刻)
    pub sql_sessions: RwLock<HashMap<String, (String, std::time::Instant)>>,
    /// token -> (username, 过期时刻)。内存只是缓存，过期必须以内存里的时刻为准，
    /// 否则进程不重启就永远有效（库里 24h 过期 + purge 都被绕过）。
    pub sessions: RwLock<HashMap<String, (String, Instant)>>,
    pub http: reqwest::Client,
    /// cluster/ns/name → (REST base URL, 取到的时刻)。
    /// Service 的 nodePort 不会频繁变化，缓存 60s 足够，避免每个请求都打 apiserver。
    pub rest_cache: RwLock<HashMap<String, (String, Instant)>>,
    /// Prometheus base URL 缓存（自动发现比较贵，5 分钟一次）。
    pub prom_cache: RwLock<Option<(String, Instant)>>,
    /// jobId → (上次累计 in, 上次累计 out, 时刻)。用于把累计吞吐换算成每秒速率。
    pub throughput_cache: RwLock<HashMap<String, (f64, f64, Instant)>>,
    /// MCP token 的令牌桶：token_id → (剩余令牌, 上次补充时刻)
    pub mcp_rate: RwLock<HashMap<String, (f64, Instant)>>,
    /// 审批事件广播：MCP 落了新审批 / 有人批了，都往这里推，前端用 SSE 收
    pub approvals_tx: tokio::sync::broadcast::Sender<String>,
    /// 索引同步单飞：后台周期同步与手动刷新不能同时跑（两次全量覆盖会互相删行）
    pub sync_lock: tokio::sync::Mutex<()>,
    /// 重接口（背压/火焰图）的短期缓存：这些请求会让 JM 采样上百次，
    /// 单个用户按 1s 轮询就能把生产作业的 JobManager 打满。
    pub heavy_cache: RwLock<HashMap<String, (serde_json::Value, Instant)>>,
    /// 已知的实际并行度：key = cluster/ns/name。detail 算出就记，列表直接复用，
    /// 避免列表为了一个数字对 51 个作业挨个打 JobManager REST。
    pub actual_parallelism: std::sync::Mutex<HashMap<String, (i64, Instant)>>,
    /// 登录失败计数：key = `ip|username`（单实例内存态；多副本需换 Redis，
    /// 但多副本本来就要先解决 watcher 单例，见 README 的"已知限制"）。
    pub login_throttle: std::sync::Mutex<HashMap<String, LoginThrottle>>,
    /// 索引最后一次成功写入的时刻（watch upsert / 全量对账）。
    /// `/api/jobs` 会把它和"现在"的差值返回，前端据此显示"索引滞后 Ns"。
    pub last_index_at: std::sync::Mutex<Option<Instant>>,
    /// 写操作"单飞"表：key = `METHOD path`（如 `POST /api/jobs/local/flink/orders/restart`）。
    ///
    /// 双击/连点/脚本重试都会命中同一个 key，第二个请求直接 409，
    /// 而不是把同一动作对集群执行两次（重启连点 3 次曾把作业打成 FAILED）。
    pub inflight: std::sync::Mutex<HashMap<String, Instant>>,
}

/// 写操作期间的占位守卫：drop 时自动释放，异常返回也不会卡住后续请求。
pub struct ActionGuard {
    st: Arc<AppState>,
    key: String,
}

impl Drop for ActionGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = self.st.inflight.lock() {
            g.remove(&self.key);
        }
    }
}

impl AppState {
    /// 记一次"索引刚被写过"，供 `/api/jobs` 暴露新鲜度。
    pub fn mark_index_fresh(&self) {
        if let Ok(mut g) = self.last_index_at.lock() {
            *g = Some(Instant::now());
        }
    }

    // ---------------------------------------------------------------- 登录限流

    /// 该 key 当前是否被锁；返回剩余秒数。
    pub fn login_locked_secs(&self, key: &str) -> Option<u64> {
        let g = self.login_throttle.lock().ok()?;
        let rec = g.get(key)?;
        let until = rec.locked_until?;
        let now = Instant::now();
        (until > now).then(|| (until - now).as_secs().max(1))
    }

    /// 记一次失败；返回"是否刚好触发锁定"。
    pub fn record_login_failure(&self, key: &str) -> bool {
        let Ok(mut g) = self.login_throttle.lock() else {
            return false;
        };
        let now = Instant::now();
        let rec = g.entry(key.to_string()).or_default();
        // 窗口过期就重新计数
        if rec
            .first_at
            .map(|t| now.duration_since(t) > LOGIN_WINDOW)
            .unwrap_or(true)
        {
            rec.fails = 0;
            rec.first_at = Some(now);
            rec.locked_until = None;
        }
        rec.fails = rec.fails.saturating_add(1);
        if rec.fails >= LOGIN_MAX_FAILS {
            rec.locked_until = Some(now + LOGIN_LOCK);
            rec.fails = 0;
            rec.first_at = Some(now);
            return true;
        }
        false
    }

    pub fn clear_login_failures(&self, key: &str) {
        if let Ok(mut g) = self.login_throttle.lock() {
            g.remove(key);
        }
    }

    // ---------------------------------------------------------------- 实际并行度

    /// 记录/读取"实际并行度"缓存（10 分钟有效）。
    pub fn put_actual_parallelism(&self, key: &str, v: i64) {
        if let Ok(mut g) = self.actual_parallelism.lock() {
            g.insert(key.to_string(), (v, Instant::now()));
        }
    }

    pub fn actual_parallelism(&self, key: &str) -> Option<i64> {
        self.actual_parallelism.lock().ok().and_then(|g| {
            g.get(key)
                .filter(|(_, at)| at.elapsed() < Duration::from_secs(600))
                .map(|(v, _)| *v)
        })
    }

    /// 索引滞后秒数；从未写过时返回 None。
    pub fn index_age_secs(&self) -> Option<u64> {
        self.last_index_at
            .lock()
            .ok()
            .and_then(|g| *g)
            .map(|at| at.elapsed().as_secs())
    }
    /// 占用一个写操作槽位。同一个 key 在 `ttl` 内只允许一个请求在跑。
    pub fn begin_action(self: &Arc<Self>, key: &str, ttl: Duration) -> Result<ActionGuard> {
        let mut g = self
            .inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 顺手清理过期项，避免 map 无限增长
        g.retain(|_, at| at.elapsed() < ttl * 4);
        if g.contains_key(key) {
            return Err(Error::Conflict(format!(
                "这个操作刚提交过（{}s 内同一动作只允许一次），请等待当前操作完成后再试",
                ttl.as_secs()
            )));
        }
        g.insert(key.to_string(), Instant::now());
        Ok(ActionGuard {
            st: self.clone(),
            key: key.to_string(),
        })
    }
}
impl AppState {
    pub async fn cluster(&self, id: &str) -> Result<Arc<Cluster>> {
        self.clusters
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("cluster {id} 未接入")))
    }

    /// 集群快照：先克隆出 (id, Arc)，再释放读锁 —— 避免遍历时持有 guard 又去 await。
    pub async fn cluster_entries(&self) -> Vec<(String, Arc<Cluster>)> {
        self.clusters
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub async fn cluster_ids(&self) -> Vec<String> {
        self.clusters.read().await.keys().cloned().collect()
    }

    /// 平台"认定存在"的集群集合：env 配置 ∪ 动态接入（库） ∪ 当前已连上的。
    ///
    /// 孤儿索引清理必须用它，不能用 `cluster_ids()`——后者只是"本次进程连上的"，
    /// 集群临时不可达（apiserver 抖动、kubeconfig 未挂载）会被误判成"集群被移除"，
    /// 从而把整簇作业索引删掉。
    pub async fn configured_cluster_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cfg.clusters.iter().map(|(id, _)| id.clone()).collect();
        if let Ok(rows) = self.store.list_clusters().await {
            out.extend(rows.into_iter().map(|r| r.0));
        }
        out.extend(self.cluster_ids().await);
        out.sort();
        out.dedup();
        out
    }

    pub async fn clusters_count(&self) -> usize {
        self.clusters.read().await.len()
    }

    /// 重接口缓存读取（默认 30s）。
    pub async fn heavy_get(&self, key: &str, ttl: Duration) -> Option<serde_json::Value> {
        let map = self.heavy_cache.read().await;
        map.get(key)
            .filter(|(_, at)| at.elapsed() < ttl)
            .map(|(v, _)| v.clone())
    }

    pub async fn heavy_put(&self, key: &str, value: serde_json::Value) {
        let mut map = self.heavy_cache.write().await;
        // 顺手做体积保护：缓存表不该无限增长
        if map.len() > 512 {
            map.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(120));
        }
        map.insert(key.to_string(), (value, Instant::now()));
    }

    pub async fn has_clusters(&self) -> bool {
        !self.clusters.read().await.is_empty()
    }

    /// Flink JobManager REST 客户端。
    ///
    /// 地址解析交给 `Cluster::rest_endpoint`：平台在集群内走 DNS，
    /// 在集群外走 Service 的 nodePort（这是踩过的坑，见 docs/data-sources.md）。
    pub async fn rest_for(&self, cluster: &str, ns: &str, name: &str) -> Result<FlinkRest> {
        let key = format!("{cluster}/{ns}/{name}");
        if let Some((base, at)) = self.rest_cache.read().await.get(&key) {
            if at.elapsed() < Duration::from_secs(60) {
                // 复用全局 reqwest::Client（连接池是 Client 级的，
                // 每次 new 一个等于每次都重新 TCP 握手）
                return Ok(FlinkRest::with_client(base.clone(), self.http.clone()));
            }
        }
        let endpoint = self.cluster(cluster).await?.rest_endpoint(ns, name).await?;
        let base = endpoint.base();
        self.rest_cache
            .write()
            .await
            .insert(key, (base.clone(), Instant::now()));
        Ok(FlinkRest::with_client(base, self.http.clone()))
    }

    /// 只解析会话 Cookie（内存优先，未命中查库）。
    async fn session_user(&self, headers: &HeaderMap) -> Option<String> {
        let token = cookie(headers, SESSION_COOKIE)?;
        {
            let map = self.sessions.read().await;
            if let Some((u, exp)) = map.get(&token) {
                if Instant::now() < *exp {
                    return Some(u.clone());
                }
            }
        }
        // 内存里没有或已过期：放到库上判断（库里 expires_at 才是权威）
        self.sessions.write().await.remove(&token);
        match self.store.session_user(&token).await {
            Ok(Some(u)) => {
                self.sessions
                    .write()
                    .await
                    .insert(token, (u.clone(), Instant::now() + SESSION_TTL));
                Some(u)
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "session lookup failed");
                None
            }
        }
    }

    /// 会话主体（角色 + 命名空间白名单）。
    ///
    /// 库里没有该账号时回落到内置管理员（env 的 admin，角色 admin）：
    /// 这样老部署升级后不需要先建表数据就能进来建第一个账号。
    pub async fn principal(&self, headers: &HeaderMap) -> Option<Principal> {
        let user = self.session_user(headers).await?;
        match self.store.get_user(&user).await {
            Ok(Some(u)) => {
                if u.disabled {
                    return None;
                }
                Some(Principal {
                    user: u.username,
                    role: u.role,
                    allow_ns: u.allow_ns,
                })
            }
            Ok(None) => Some(Principal {
                user,
                role: "admin".into(),
                allow_ns: Vec::new(),
            }),
            Err(e) => {
                warn!(error = %e, "principal lookup failed");
                None
            }
        }
    }

    pub async fn require_admin(&self, headers: &HeaderMap) -> Result<Principal> {
        let p = self.principal(headers).await.ok_or(Error::Unauthorized)?;
        if !p.is_admin() {
            return Err(Error::Forbidden(format!(
                "需要 admin 角色（当前 {} 是 {}）",
                p.user, p.role
            )));
        }
        Ok(p)
    }

    /// 宽松版：Cookie 或 Bearer 都认，失败一律 None（适合"顺便记个 actor"的场景）。
    pub async fn user(&self, headers: &HeaderMap) -> Option<String> {
        if let Some(u) = self.session_user(headers).await {
            return Some(u);
        }
        match crate::mcp_guard::authenticate(self, headers).await {
            Ok(auth) => auth.map(|a| a.actor),
            Err(e) => {
                warn!(error = %e, "mcp token rejected");
                None
            }
        }
    }

    /// 严格版：没有凭据 → 401；Token 无效 → 401；**超配额 → 429**（不要吞成 401，
    /// 否则调用方会以为 Token 坏了，实际只是打太快）。
    pub async fn require_user(&self, headers: &HeaderMap) -> Result<String> {
        if let Some(u) = self.session_user(headers).await {
            return Ok(u);
        }
        match crate::mcp_guard::authenticate(self, headers).await {
            Ok(Some(auth)) => Ok(auth.actor),
            Ok(None) => Err(Error::Unauthorized),
            Err(e) => Err(e),
        }
    }

    /// Prometheus 地址：配置优先；否则自动发现（读集群里的 Service）。
    ///
    /// 结果进程内缓存 60s：大盘一次筛选会连打 9 个查询，不必每个都读一次库/探一次 Service。
    /// 设置页保存会主动失效（见 `invalidate_prom_cache`），所以热生效不受影响。
    pub async fn prometheus_base(&self) -> Result<String> {
        if let Some((base, at)) = self.prom_cache.read().await.as_ref() {
            if at.elapsed() < Duration::from_secs(60) {
                return Ok(base.clone());
            }
        }
        let resolved = self.resolve_prometheus_base().await?;
        *self.prom_cache.write().await = Some((resolved.clone(), Instant::now()));
        Ok(resolved)
    }

    /// 设置变更后立刻失效 Prometheus 地址缓存。
    pub async fn invalidate_prom_cache(&self) {
        *self.prom_cache.write().await = None;
    }

    async fn resolve_prometheus_base(&self) -> Result<String> {
        // 设置页保存的地址优先（热生效），其次环境变量
        if let Some(u) = self
            .store
            .get_setting("prometheus_url")
            .await
            .filter(|v| !v.is_empty())
        {
            return Ok(u.trim_end_matches('/').to_string());
        }
        if let Some(u) = &self.cfg.prometheus_url {
            return Ok(u.trim_end_matches('/').to_string());
        }
        for (_, c) in self.cluster_entries().await {
            for cand in c.find_prometheus().await? {
                if let Some(base) = c.prom_base_url(&cand) {
                    return Ok(base);
                }
            }
        }
        Err(Error::NotFound(
            "没找到可访问的 Prometheus：可用 FLINKOPS_PROMETHEUS_URL 显式指定，或确认集群里有 NodePort 暴露的 prometheus Service"
                .into(),
        ))
    }

    /// 写审计。失败只记日志，绝不影响主请求。
    #[allow(clippy::too_many_arguments)]
    pub fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        cluster: &str,
        ns: &str,
        result: &str,
        detail: Option<String>,
        ip: Option<String>,
    ) {
        let entry = AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            ts: now_rfc3339(),
            actor: actor.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            cluster: cluster.to_string(),
            namespace: ns.to_string(),
            result: result.to_string(),
            detail,
            ip,
        };
        let store = self.store.clone();
        tokio::spawn(async move {
            if let Err(e) = store.audit(&entry).await {
                warn!(error = %e, "audit write failed");
            }
        });
    }
}

/// 从 Cookie 头里取指定键。
pub fn cookie(headers: &HeaderMap, key: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}
