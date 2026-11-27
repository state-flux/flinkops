//! 环境配置。
//!
//! 约定：所有配置只从环境变量读，不在代码里写死；缺省值必须能"零配置跑起来"。
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub enum ClusterSource {
    InCluster,
    /// kubeconfig 文件路径（跨集群接入）
    Kubeconfig(String),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub store_dsn: String,
    pub web_dir: String,
    pub clusters: Vec<(String, ClusterSource)>,
    pub audit_retention_days: i64,
    pub db_max_mb: u64,
    pub prometheus_url: Option<String>,
    pub admin_user: String,
    pub admin_password: String,
    pub readonly: bool,
    /// 会话 Cookie 是否加 `Secure`：`FLINKOPS_COOKIE_SECURE=true|false`，
    /// 不设则按请求的 `X-Forwarded-Proto` 自动判断（TLS 终止在 Ingress 上）。
    pub cookie_secure: Option<bool>,
    /// 动态接入时"粘贴 kubeconfig"落盘的位置（目录 0700、文件 0600）。
    /// `FLINKOPS_CLUSTERS_DIR`，默认放在 SQLite 同级的 `clusters/`（其它后端用 `./data/clusters`）。
    pub clusters_dir: std::path::PathBuf,
    /// 启动后自动打开浏览器：`FLINKOPS_OPEN_BROWSER=1`。
    /// 便携包 / 安装包默认打开，双击 exe 就能直接用。
    pub open_browser: bool,
}

impl Config {
    pub fn from_env() -> Self {
        // 先加载 .env：便携包/安装包靠它做到"双击即用"，而且 FLINKOPS_STORE / FLINKOPS_BIND
        // 也在 .env 里，所以必须比任何读取都早。
        load_dotenv();

        let store_dsn = resolve_store_dsn(&env_or(
            "FLINKOPS_STORE",
            "sqlite://./data/flinkops.db?mode=rwc",
        ));
        let bind = std::env::var("FLINKOPS_BIND")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("valid default bind"));

        let clusters = match std::env::var("FLINKOPS_CLUSTERS") {
            Ok(raw) => raw.split(',').filter_map(parse_cluster).collect(),
            Err(_) => default_clusters(),
        };

        Self {
            bind,
            store_dsn: store_dsn.clone(),
            // 前端构建产物默认路径；相对路径会从 CWD 向上查找，
            // 这样从 crates/flinkops-api 目录跑也能找到 apps/web/dist
            web_dir: resolve_web_dir(&env_or("FLINKOPS_WEB_DIR", "./apps/web/dist")),
            clusters,
            audit_retention_days: env_parse("FLINKOPS_AUDIT_RETENTION_DAYS", 90),
            db_max_mb: env_parse("FLINKOPS_DB_MAX_MB", 512),
            prometheus_url: std::env::var("FLINKOPS_PROMETHEUS_URL")
                .ok()
                .filter(|v| !v.is_empty()),
            admin_user: env_or("FLINKOPS_ADMIN_USER", "admin"),
            admin_password: env_or("FLINKOPS_ADMIN_PASSWORD", "admin"),
            readonly: env_flag("FLINKOPS_READONLY"),
            clusters_dir: resolve_clusters_dir(&store_dsn),
            cookie_secure: std::env::var("FLINKOPS_COOKIE_SECURE")
                .ok()
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")),
            open_browser: env_flag("FLINKOPS_OPEN_BROWSER"),
        }
    }
}

/// 没显式配置集群时的默认链：
/// 1. 仓库内 `.secrets/kubeconfig.yaml`（本地开发约定，已在 .gitignore）
/// 2. 回退 in-cluster（生产 Pod 里就是这个）
fn default_clusters() -> Vec<(String, ClusterSource)> {
    if let Some(path) = find_up(&[".secrets/kubeconfig.yaml"]) {
        return vec![("local".to_string(), ClusterSource::Kubeconfig(path))];
    }
    vec![("local".to_string(), ClusterSource::InCluster)]
}

/// 从当前目录向上最多 5 层查找相对路径。
/// 解决"IDE 用 crate 目录当 CWD，相对路径找不到"的常见问题。
fn find_up(candidates: &[&str]) -> Option<String> {
    let mut cur = std::env::current_dir().ok()?;
    for _ in 0..5 {
        for c in candidates {
            let p = cur.join(c);
            if p.exists() {
                return Some(normalize(&p));
            }
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// 去掉路径里的 `.` / `..`，让日志与 DSN 干净。
fn normalize(p: &std::path::Path) -> String {
    p.components()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .to_string()
}

/// 向上找 workspace 根（Cargo.toml 里含 [workspace]）。
fn workspace_root() -> Option<std::path::PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    for _ in 0..5 {
        let manifest = cur.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if text.contains("[workspace]") {
                return Some(cur);
            }
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// sqlite 相对路径基于 workspace 根解析。
/// 否则从 crates/flinkops-api 目录启动时，会在那里又建一个 data/flinkops.db。
/// 粘贴进来的 kubeconfig 落在哪：显式 env 优先，否则 SQLite 同级 `clusters/`。
fn resolve_clusters_dir(store_dsn: &str) -> std::path::PathBuf {
    if let Ok(v) = std::env::var("FLINKOPS_CLUSTERS_DIR") {
        if !v.trim().is_empty() {
            return std::path::PathBuf::from(v.trim());
        }
    }
    // sqlite:///D:/x/data/flinkops.db?mode=rwc → D:/x/data/clusters
    let path = store_dsn
        .trim_start_matches("sqlite://")
        .split('?')
        .next()
        .unwrap_or("")
        .trim_start_matches('/');
    let parent = std::path::Path::new(path).parent();
    match parent {
        Some(d) if !d.as_os_str().is_empty() => d.join("clusters"),
        _ => std::path::PathBuf::from("./data/clusters"),
    }
}

fn resolve_store_dsn(dsn: &str) -> String {
    let Some(rest) = dsn.strip_prefix("sqlite://") else {
        return dsn.to_string();
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    if path.starts_with(':') || !(path.starts_with("./") || path.starts_with("../")) {
        return dsn.to_string();
    }
    // 相对 DSN 的基准：在仓库里开发时以 workspace 根为准；便携包/安装包（exe 不在仓库内）
    // 以 exe 所在目录为准，这样双击启动后数据落在程序旁边。
    let base = match workspace_root() {
        Some(root) if exe_inside(&root) => Some(root),
        _ => exe_dir().or_else(workspace_root),
    };
    let Some(root) = base else {
        return dsn.to_string();
    };
    let joined = normalize(&root.join(path));
    let mut abs = joined.replace('\\', "/");
    if !abs.starts_with('/') {
        abs = format!("/{abs}");
    }
    match query {
        Some(q) => format!("sqlite://{abs}?{q}"),
        None => format!("sqlite://{abs}"),
    }
}

/// 静态目录解析：显式路径 → CWD 向上查找 → exe 同级（便携包）→ 系统安装路径。
fn resolve_web_dir(raw: &str) -> String {
    let p = std::path::Path::new(raw);
    if p.exists() {
        // 相对路径转成绝对路径，避免后续 CWD 变化（服务里不再依赖 CWD）
        if p.is_absolute() {
            return normalize(p);
        }
        if let Ok(cwd) = std::env::current_dir() {
            return normalize(&cwd.join(p));
        }
        return raw.to_string();
    }
    if let Some(p) = find_up(&[raw]) {
        return p;
    }
    if let Some(dir) = exe_dir() {
        for cand in [
            dir.join("web"),                      // 便携包：exe 与 web/ 同级
            dir.join("../web"),                   // exe 在 bin/ 里
            dir.join("apps/web/dist"),            // 源码树里直接跑 release 二进制
            dir.join("../lib/flinkops/web"),      // .deb：/usr/bin/flinkops → /usr/lib/flinkops/web
        ] {
            if cand.exists() {
                return normalize(&cand);
            }
        }
    }
    raw.to_string()
}

/// exe 是否就位于该 workspace 内（决定相对路径以谁为基准）。
fn exe_inside(root: &std::path::Path) -> bool {
    match std::env::current_exe() {
        Ok(exe) => exe.starts_with(root),
        Err(_) => false,
    }
}

/// exe 所在目录（便携包 / 安装包用它定位 web、.env、data）。
fn exe_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent().map(|p| p.to_path_buf())
}

/// `.env` 的位置：先按 CWD 向上找（开发时），再找 exe 同级（双击启动时 CWD 可能不是程序目录）。
fn dotenv_path() -> Option<String> {
    if let Some(p) = find_up(&[".env"]) {
        return Some(p);
    }
    let candidate = exe_dir()?.join(".env");
    candidate.exists().then(|| normalize(&candidate))
}

/// 极简 .env 解析（KEY=VALUE，# 注释）；不覆盖已存在的环境变量。
///
/// 刻意不引 dotenvy：只有 20 行，且行为可预测。
fn load_dotenv() {
    let Some(path) = dotenv_path() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() || std::env::var_os(key).is_some() {
            continue;
        }
        let value = v.trim().trim_matches('"').trim_matches('\'');
        std::env::set_var(key, value);
    }
}

fn parse_cluster(item: &str) -> Option<(String, ClusterSource)> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    let (id, src) = item.split_once('=')?;
    let id = id.trim().to_string();
    let src = src.trim();
    if id.is_empty() || src.is_empty() {
        return None;
    }
    let source = if src.eq_ignore_ascii_case("incluster") {
        ClusterSource::InCluster
    } else {
        ClusterSource::Kubeconfig(src.to_string())
    };
    Some((id, source))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cluster_list() {
        let a = parse_cluster("prod=incluster").unwrap();
        assert_eq!(a.0, "prod");
        assert!(matches!(a.1, ClusterSource::InCluster));

        let b = parse_cluster("dev=/tmp/kubeconfig").unwrap();
        assert_eq!(b.0, "dev");
        assert!(matches!(b.1, ClusterSource::Kubeconfig(_)));

        assert!(parse_cluster("  ").is_none());
        assert!(parse_cluster("no-equals").is_none());
    }
}
