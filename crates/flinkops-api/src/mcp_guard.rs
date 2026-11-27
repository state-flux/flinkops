//! MCP 的认证与边界（见 `docs/mcp-spec.md`）。
//!
//! 一句话：**机器用 Bearer Token 进来，能力被 scope / 资源范围 / QPS / 一次性审批四道闸门卡住。**
//!
//! 为什么不做成中间件：范围校验要看 `namespace` / `name` 这两个路径参数，
//! 而 axum 的 Path 只有 handler 拿得到。所以这里提供两个薄函数，由 handler 显式调用：
//!
//! - `require_user`（state.rs）：会话 Cookie 或 Bearer Token 都能过，返回 actor
//! - `gate_write`：写操作专用，检查 scope + 资源范围 + 一次性审批
//!
//! 只读接口的范围检查用 `check_job_scope`（有 Token 才校验，Cookie 会话不受影响）。

use std::time::Instant;

use axum::http::HeaderMap;
use flinkops_store::McpToken;
use flinkops_types::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::state::AppState;

pub const TOKEN_PREFIX: &str = "fko_pat_";
pub const APPROVAL_HEADER: &str = "x-flinkops-approval";
/// 审批窗口：契约 §4.2 —— 超过 300s 的审批一律作废，避免"昨天批的今天还能用"。
pub const APPROVAL_TTL_SECS: i64 = 300;

/// 写操作闸门的薄封装：浏览器会话直接放行，Bearer Token 走完整校验。
///
/// 所有会改集群的端点都必须先调它（作业/快照/会话/SQL 部署…），
/// 否则"只读 Token + 限定命名空间"这两条承诺会被绕过。
pub async fn gate(
    st: &std::sync::Arc<AppState>,
    headers: &HeaderMap,
    tool: &str,
    ns: &str,
    name: &str,
    args: Value,
    need_approval: bool,
) -> Result<()> {
    gate_write(st, headers, tool, ns, name, &args, need_approval)
        .await
        .map(|_| ())
}

pub fn approval_expired(created_at: &str) -> bool {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(ts) => {
            (chrono::Utc::now() - ts.with_timezone(&chrono::Utc)).num_seconds() > APPROVAL_TTL_SECS
        }
        Err(_) => true, // 解析不了就当过期，宁可让人重批
    }
}

/// 从 `Authorization: Bearer fko_pat_<id>_<secret>` 里取出 (id, secret)。
pub fn bearer_parts(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let body = rest.trim().strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = body.split_once('_')?;
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    Some((id.to_string(), secret.to_string()))
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// 审批参数哈希：`sha256(canonical_json(args))`。
/// canonical 化只做"键排序 + 去空白"，足够防"批 A 执行 B"。
pub fn args_hash(args: &Value) -> String {
    sha256_hex(&canonical(args))
}

fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// 已认证的 MCP 调用者。
#[derive(Clone, Debug)]
pub struct McpAuth {
    pub token: McpToken,
    pub actor: String,
}

pub fn is_mcp(headers: &HeaderMap) -> bool {
    bearer_parts(headers).is_some()
}

/// 校验 token 本身：存在、未吊销、未过期、QPS 未超限。
pub async fn authenticate(st: &AppState, headers: &HeaderMap) -> Result<Option<McpAuth>> {
    let Some(token) = peek(st, headers).await? else {
        return Ok(None);
    };
    if !take_rate(st, &token).await {
        return Err(Error::Conflict(
            "mcp rate limited（retryAfterMs≈200）".into(),
        ));
    }
    st.store.touch_mcp_token(&token.id).await;
    Ok(Some(McpAuth {
        actor: format!("mcp:{}", token.name),
        token,
    }))
}

/// 只校验不扣配额：供 `check_job_scope` / `gate_write` 复用（
/// 避免同一次请求被 `require_user` 扣一次、闸门再扣一次）。
pub async fn peek(st: &AppState, headers: &HeaderMap) -> Result<Option<McpToken>> {
    let Some((id, secret)) = bearer_parts(headers) else {
        return Ok(None);
    };
    let Some(token) = st.store.get_mcp_token(&id).await? else {
        return Ok(None);
    };
    if token.revoked_at.is_some() {
        return Ok(None);
    }
    if let Some(exp) = &token.expires_at {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(exp) {
            if ts.with_timezone(&chrono::Utc) < chrono::Utc::now() {
                return Ok(None);
            }
        }
    }
    let Some(hash) = st.store.mcp_token_secret_hash(&id).await? else {
        return Ok(None);
    };
    if !constant_time_eq(&hash, &sha256_hex(&secret)) {
        return Ok(None);
    }
    Ok(Some(token))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 令牌桶：每个 token 按 `qps` 补充，最多存 1 秒的量。
async fn take_rate(st: &AppState, token: &McpToken) -> bool {
    let qps = token.qps.max(1) as f64;
    let mut map = st.mcp_rate.write().await;
    let now = Instant::now();
    let entry = map.entry(token.id.clone()).or_insert((qps, now));
    let elapsed = now.duration_since(entry.1).as_secs_f64();
    entry.0 = (entry.0 + elapsed * qps).min(qps);
    entry.1 = now;
    if entry.0 >= 1.0 {
        entry.0 -= 1.0;
        true
    } else {
        false
    }
}

/// 简化 glob：支持 `*`（任意串）与完全相等。
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.split_once('*') {
        None => pattern == value,
        Some((head, tail)) => value.starts_with(head) && value.ends_with(tail),
    }
}

pub fn scope_ok(token: &McpToken, scope: &str) -> bool {
    token.scopes.iter().any(|s| s == scope || s == "admin")
}

/// 该 Token 是否被允许看到/操作这个命名空间（不看具体作业）。
pub fn ns_allowed(token: &McpToken, ns: &str) -> bool {
    if token.allow_ns.is_empty() && token.allow_jobs.is_empty() {
        return false;
    }
    token.allow_ns.iter().any(|p| glob_match(p, ns))
}

/// 该 Token 是否被允许操作 `namespace/name`。
pub fn job_allowed(token: &McpToken, ns: &str, name: &str) -> bool {
    // 空 = 无权（契约 §2.3）：既没给命名空间也没给作业就什么都碰不到
    if token.allow_ns.is_empty() && token.allow_jobs.is_empty() {
        return false;
    }
    let ns_ok = token.allow_ns.iter().any(|p| glob_match(p, ns));
    if !ns_ok {
        return false;
    }
    if token.allow_jobs.is_empty() {
        return true; // 只限定到命名空间
    }
    let target = format!("{ns}/{name}");
    token.allow_jobs.iter().any(|p| glob_match(p, &target))
}

/// 只读接口的资源范围检查：没有 Bearer 就跳过（浏览器会话）。
pub async fn check_job_scope(
    st: &AppState,
    headers: &HeaderMap,
    ns: &str,
    name: &str,
) -> Result<()> {
    if !is_mcp(headers) {
        return Ok(());
    }
    if let Some(token) = peek(st, headers).await? {
        if !scope_ok(&token, "read") {
            return Err(Error::Forbidden("token 缺少 read scope".into()));
        }
        if !job_allowed(&token, ns, name) {
            return Err(Error::Forbidden(format!("token 无权访问 {ns}/{name}")));
        }
    }
    Ok(())
}

/// 写操作闸门：scope → 资源范围 → 一次性审批。
///
/// 没有 Bearer 时直接放行（浏览器会话仍走原来的 `readonly` 检查）。
/// 有 Bearer 时：
/// - 缺 `operate`/`admin` → Forbidden
/// - 目标越界 → Forbidden
/// - 没带审批 token / 带了但对不上（工具名或参数哈希不一致） → 先落一条 pending 审批，返回 Conflict
/// - 带了且匹配 → 核销，放行
pub async fn gate_write(
    st: &AppState,
    headers: &HeaderMap,
    tool: &str,
    ns: &str,
    name: &str,
    args: &Value,
    need_approval: bool,
) -> Result<Option<McpAuth>> {
    if !is_mcp(headers) {
        return Ok(None);
    }
    let auth = peek(st, headers).await?.map(|token| McpAuth {
        actor: format!("mcp:{}", token.name),
        token,
    });
    let Some(auth) = auth else {
        return Err(Error::Unauthorized);
    };
    let scope = if tool == "flinkops_delete_job"
        || tool == "flinkops_create_job"
        || tool == "flinkops_delete_snapshot"
    {
        "admin"
    } else {
        "operate"
    };
    if !scope_ok(&auth.token, scope) {
        return Err(Error::Forbidden(format!("token 缺少 {scope} scope")));
    }
    if !job_allowed(&auth.token, ns, name) {
        return Err(Error::Forbidden(format!("token 无权操作 {ns}/{name}")));
    }

    let want_hash = args_hash(args);
    let target = format!("{ns}/{name}");

    if !need_approval {
        return Ok(Some(auth));
    }

    if let Some(raw_token) = headers.get(APPROVAL_HEADER).and_then(|v| v.to_str().ok()) {
        let hash = sha256_hex(raw_token.trim());
        let Some(approval) = st.store.consume_mcp_approval(&hash).await? else {
            return Err(Error::Forbidden(
                "approval_invalid：审批已使用或已过期".into(),
            ));
        };
        if approval_expired(&approval.created_at) {
            return Err(Error::Forbidden(format!(
                "approval_expired：审批超过 {APPROVAL_TTL_SECS}s，请重新发起"
            )));
        }
        if approval.tool != tool || approval.args_hash != want_hash || approval.target != target {
            return Err(Error::Forbidden(
                "approval_invalid：审批的工具/参数/目标与本次调用不一致".into(),
            ));
        }
        return Ok(Some(auth));
    }

    // 没带审批：落一条 pending，交给网页审批中心
    let id = format!("ap_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    st.store
        .insert_mcp_approval(&id, tool, &want_hash, &target, &auth.actor, &auth.token.id)
        .await?;
    // 推给网页（SSE），审批中心立即可见，不用等轮询
    let _ = st.approvals_tx.send(
        serde_json::json!({
            "type": "pending",
            "id": id,
            "tool": tool,
            "target": target,
            "actor": auth.actor,
        })
        .to_string(),
    );
    Err(Error::Conflict(format!("approval_required:{id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn glob_rules() {
        assert!(glob_match("*", "flink"));
        assert!(glob_match("flink*", "flink-prod"));
        assert!(!glob_match("flink", "flink-prod"));
        assert!(glob_match("flink/etl-*", "flink/etl-demo"));
        assert!(!glob_match("flink/etl-*", "flink/other-job"));
    }

    #[test]
    fn args_hash_is_stable_across_key_order() {
        let a = json!({"name": "x", "ns": "flink"});
        let b = json!({"ns": "flink", "name": "x"});
        assert_eq!(args_hash(&a), args_hash(&b));
        let c = json!({"ns": "flink", "name": "y"});
        assert_ne!(args_hash(&a), args_hash(&c));
    }

    #[test]
    fn token_parts() {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            "Bearer fko_pat_abc_secret".parse().unwrap(),
        );
        assert_eq!(bearer_parts(&h), Some(("abc".into(), "secret".into())));
        let mut h2 = HeaderMap::new();
        h2.insert("authorization", "Bearer nope".parse().unwrap());
        assert_eq!(bearer_parts(&h2), None);
    }
}
