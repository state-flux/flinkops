//! 启动装配：只做"接线"，不含业务逻辑（业务在 routes/*，领域在 flinkops-core）。
use std::collections::HashMap;
use std::sync::Arc;

use flinkops_api::config::{ClusterSource, Config};
use flinkops_api::routes;
use flinkops_api::AppState;
use flinkops_k8s::Cluster;
use flinkops_store::Store;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls 0.23：kube 与 reqwest 同时依赖时 feature 互相覆盖，
    // 必须显式安装 CryptoProvider，否则首个 TLS 握手 panic。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let cfg = Config::from_env();
    info!(
        bind = %cfg.bind,
        store = %cfg.store_dsn,
        web = %cfg.web_dir,
        clusters = cfg.clusters.len(),
        "starting flinkops"
    );

    // 1) 存储
    let store = Store::connect(&cfg.store_dsn).await?;
    store.migrate().await?;
    info!(backend = store.dialect().as_str(), "metadata store ready");
    store.seed_default_profile().await?;

    // 2) 集群（失败不阻塞启动：至少静态页与只读视图能起来）
    let mut clusters = HashMap::new();
    // 保存接入来源：重连时按原路重建 client
    let mut sources = HashMap::new();
    for (id, src) in &cfg.clusters {
        sources.insert(id.clone(), src.clone());
        let connected = match src {
            ClusterSource::InCluster => Cluster::try_default(id.clone(), cfg.readonly).await,
            ClusterSource::Kubeconfig(path) => {
                Cluster::from_kubeconfig(id.clone(), path, cfg.readonly).await
            }
        };
        match connected {
            Ok(c) => {
                info!(cluster = %id, "cluster connected");
                clusters.insert(id.clone(), Arc::new(c));
            }
            Err(e) => warn!(cluster = %id, error = %e, "cluster unavailable"),
        }
    }

    // 2.5) 合并库里持久化的动态集群（env 优先，同 id 不覆盖）
    if let Ok(stored) = store.list_clusters().await {
        for (id, kind, path, ro) in stored {
            if clusters.contains_key(&id) {
                continue;
            }
            let src = if kind == "kubeconfig" {
                match path {
                    Some(p) => ClusterSource::Kubeconfig(p),
                    None => continue,
                }
            } else {
                ClusterSource::InCluster
            };
            let connected = match &src {
                ClusterSource::InCluster => Cluster::try_default(id.clone(), ro).await,
                ClusterSource::Kubeconfig(p) => Cluster::from_kubeconfig(id.clone(), p, ro).await,
            };
            match connected {
                Ok(c) => {
                    info!(cluster = %id, "cluster restored from store");
                    clusters.insert(id.clone(), Arc::new(c));
                    sources.insert(id.clone(), src);
                }
                Err(e) => warn!(cluster = %id, error = %e, "stored cluster unavailable"),
            }
        }
    }

    let (approvals_tx, _) = tokio::sync::broadcast::channel::<String>(64);
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        store,
        clusters: tokio::sync::RwLock::new(clusters),
        cluster_sources: tokio::sync::RwLock::new(sources),
        sql_sessions: tokio::sync::RwLock::new(HashMap::new()),
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        rest_cache: tokio::sync::RwLock::new(HashMap::new()),
        throughput_cache: tokio::sync::RwLock::new(HashMap::new()),
        mcp_rate: tokio::sync::RwLock::new(HashMap::new()),
        approvals_tx,
        prom_cache: tokio::sync::RwLock::new(None),
        sync_lock: tokio::sync::Mutex::new(()),
        heavy_cache: tokio::sync::RwLock::new(HashMap::new()),
        inflight: std::sync::Mutex::new(HashMap::new()),
        actual_parallelism: std::sync::Mutex::new(HashMap::new()),
        login_throttle: std::sync::Mutex::new(HashMap::new()),
        last_index_at: std::sync::Mutex::new(None),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?,
    });

    // 3) 启动时同步一次作业索引
    if let Err(e) = routes::jobs::sync_clusters(&state, None, None, true).await {
        warn!(error = %e, "initial sync failed");
    }

    // 3.2) 每个集群挂 watch：作业变化实时进索引（轮询只做兜底对账）
    for (id, c) in state.cluster_entries().await {
        let _ = routes::jobs::spawn_cluster_watcher(state.clone(), id.clone(), c);
        info!(cluster = %id, "job watcher started");
    }

    // 3.5) 后台：每 5 分钟对账一次索引（watch 断流期间兜底）
    //
    // 主路径是 watch（秒级）；这里只是"万一 watch 长时间断了"的保险，
    // 所以从 30s 放宽到 300s，减少无谓的全量 list。
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                match routes::jobs::sync_clusters(&st, None, None, false).await {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(synced = n, "periodic job index resync"),
                    Err(e) => warn!(error = %e, "periodic sync failed"),
                }
            }
        });
    }

    // 4) 后台：审计保留期清理 + DB 体积检查
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                // 保留期优先读库（可在设置页热改），没有则用环境变量
                let retention = st
                    .store
                    .get_setting("audit_retention_days")
                    .await
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(st.cfg.audit_retention_days);
                let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                match st.store.purge_audit(&cutoff).await {
                    Ok(n) if n > 0 => info!(deleted = n, "audit retention sweep"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "audit purge failed"),
                }
                match st.store.purge_sessions().await {
                    Ok(n) if n > 0 => info!(deleted = n, "expired sessions purged"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "session purge failed"),
                }
                if let Some(bytes) = st.store.db_bytes() {
                    let max_mb = st
                        .store
                        .get_setting("db_max_mb")
                        .await
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(st.cfg.db_max_mb);
                    let limit = max_mb * 1024 * 1024;
                    if bytes > limit {
                        warn!(bytes, limit, "database over configured max size");
                    }
                }
            }
        });
    }

    // 4.5) 告警求值：每 60s 扫一次作业索引（规则命中才写事件，去重）
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                let n = routes::alerts::evaluate(&st).await;
                if n > 0 {
                    info!(events = n, "alert rules evaluated");
                }
            }
        });
    }

    // 5) 路由 + 监听
    let app = routes::build(state);
    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // 端口冲突是最常见的"IDE 里点两次运行"，这里给可读提示，
            // 而不是丢 30 层 backtrace 让人自己猜。
            eprintln!(
                "\n无法监听 {}：端口已被占用。\n\
                 \n\
                 多半是已经有一个实例在跑（IDE 里重复点运行，或上次没停掉）。\n\
                 \n\
                 排查：\n\
                 \x20 Windows     netstat -ano | findstr :{port}   然后 taskkill /PID <pid> /F\n\
                 \x20 macOS/Linux lsof -i :{port}\n\
                 \n\
                 换端口启动：FLINKOPS_BIND=127.0.0.1:18080\n",
                cfg.bind,
                port = cfg.bind.port()
            );
            std::process::exit(2);
        }
        Err(e) => return Err(anyhow::anyhow!("无法监听 {}: {e}", cfg.bind)),
    };
    info!(addr = %cfg.bind, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 优雅停机：收到 Ctrl-C / SIGTERM 后停止接新连接，等在途请求收尾。
///
/// 没有它时，滚动更新/重启会把正在写审计、正在 apply 的请求直接掐断。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining…");
    // 给后台审计/同步任务一点时间落库
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}
