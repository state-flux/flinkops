//! 登录 / 登出 / 当前用户 / 改密码 / 两步验证（TOTP）。
//!
//! 安全基线（2026-11-27 加固）：
//! - 密码 **argon2id** 存储，老库的 sha256 在登录成功时自动升级；
//! - 登录失败按 `IP+账号` 计数，5 次 / 10 分钟 → 锁 5 分钟（429 + Retry-After）；
//! - 会话 Cookie 带 `HttpOnly; SameSite=Lax`，TLS 场景补 `Secure`；
//! - 另发一个非 HttpOnly 的 `flinkops_csrf`，写操作必须回带 `X-CSRF-Token`；
//! - 可选 TOTP 两步验证（登录时要求 6 位动态码）。
//!
//! 会话本身仍是"DB 落库 + 内存缓存"，多副本共享靠 DB（见 `state.rs`）。
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::error::{ApiError, ApiResult};
use crate::state::{cookie, AppState, CSRF_COOKIE, LOGIN_MAX_FAILS, SESSION_COOKIE};
use flinkops_types::Error;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// 开了两步验证的账号必填
    #[serde(default)]
    pub totp: Option<String>,
}

/// 客户端 IP：优先反代头，取不到就 "unknown"（限流退化成按账号计数）。
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// 是否给 Cookie 加 `Secure`：显式配置优先，否则看反代是否终止了 TLS。
fn use_secure(st: &AppState, headers: &HeaderMap) -> bool {
    if let Some(v) = st.cfg.cookie_secure {
        return v;
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn set_cookie(name: &str, value: &str, max_age: i64, http_only: bool, secure: bool) -> String {
    format!(
        "{name}={value}; Path=/;{} SameSite=Lax; Max-Age={max_age}{}",
        if http_only { " HttpOnly;" } else { "" },
        if secure { "; Secure" } else { "" }
    )
}

pub async fn login(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Response {
    let ip = client_ip(&headers);
    let throttle_key = format!("{ip}|{}", req.username);
    if let Some(secs) = st.login_locked_secs(&throttle_key) {
        st.audit(
            &req.username,
            "auth.login",
            "/api/login",
            "",
            "",
            "denied",
            Some(format!("locked，剩余 {secs}s")),
            None,
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, secs.to_string())],
            Json(json!({
                "error": {
                    "code": "too_many_attempts",
                    "message": format!("登录失败次数过多，请 {secs} 秒后再试")
                }
            })),
        )
            .into_response();
    }

    // 账号优先查库（多租户）；库里没有时回落到环境变量里的内置管理员，
    // 保证老部署升级后还能用 admin/admin 进来建账号。
    let stored = st.store.get_user(&req.username).await.ok().flatten();
    let mut rehashed = false;
    let (authed, totp_secret) = match &stored {
        Some(u) => {
            let ok = !u.disabled
                && !u.password_hash.is_empty()
                && flinkops_store::verify_password(&req.password, &u.password_hash);
            if ok && flinkops_store::needs_rehash(&u.password_hash) {
                // 老 sha256 → argon2id：登录成功时静默升级
                let hash = flinkops_store::hash_password(&req.password);
                if let Err(e) = st.store.set_password_hash(&u.username, &hash).await {
                    tracing::warn!(error = %e, "password rehash failed");
                } else {
                    rehashed = true;
                }
            }
            (ok, u.totp_secret.clone())
        }
        None => (
            req.username == st.cfg.admin_user && req.password == st.cfg.admin_password,
            String::new(),
        ),
    };

    // 两步验证：密码对了还要动态码
    let mut totp_ok = true;
    if authed && !totp_secret.is_empty() {
        totp_ok = req
            .totp
            .as_deref()
            .map(|code| crate::totp::verify(&totp_secret, code))
            .unwrap_or(false);
    }

    if !authed || !totp_ok {
        let locked = st.record_login_failure(&throttle_key);
        st.audit(
            &req.username,
            "auth.login",
            "/api/login",
            "",
            "",
            "denied",
            Some(if !authed {
                "bad_credentials".to_string()
            } else {
                "bad_totp".to_string()
            }),
            None,
        );
        let (code, msg) = if !authed {
            ("bad_credentials", "用户名或密码错误")
        } else {
            ("totp_invalid", "两步验证码不正确")
        };
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": code, "message": msg } })),
        )
            .into_response();
        if locked {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, "300".parse().unwrap());
        }
        return resp;
    }

    st.clear_login_failures(&throttle_key);

    let token = uuid::Uuid::new_v4().to_string();
    let csrf = uuid::Uuid::new_v4().simple().to_string();
    if let Err(e) = st.store.create_session(&token, &req.username, 24).await {
        tracing::warn!(error = %e, "session persist failed, fallback to memory only");
    }
    st.sessions.write().await.insert(
        token.clone(),
        (
            req.username.clone(),
            std::time::Instant::now() + crate::state::SESSION_TTL,
        ),
    );
    st.audit(
        &req.username,
        "auth.login",
        "/api/login",
        "",
        "",
        "ok",
        Some(format!(
            "ip={ip} mfa={}{}",
            !totp_secret.is_empty(),
            if rehashed { " rehash=argon2id" } else { "" }
        )),
        None,
    );

    let secure = use_secure(&st, &headers);
    let mut cookies = HeaderMap::new();
    cookies.append(
        header::SET_COOKIE,
        set_cookie(SESSION_COOKIE, &token, 86_400, true, secure)
            .parse()
            .expect("cookie header"),
    );
    cookies.append(
        header::SET_COOKIE,
        // 非 HttpOnly：前端要读出来放进 X-CSRF-Token
        set_cookie(CSRF_COOKIE, &csrf, 86_400, false, secure)
            .parse()
            .expect("cookie header"),
    );
    (
        StatusCode::OK,
        cookies,
        Json(json!({
            "user": req.username,
            "csrf": csrf,
            "failedAttemptsLeft": LOGIN_MAX_FAILS,
        })),
    )
        .into_response()
}

pub async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers, SESSION_COOKIE) {
        st.sessions.write().await.remove(&token);
        if let Err(e) = st.store.delete_session(&token).await {
            tracing::warn!(error = %e, "session delete failed");
        }
    }
    let secure = use_secure(&st, &headers);
    let mut cookies = HeaderMap::new();
    cookies.append(
        header::SET_COOKIE,
        set_cookie(SESSION_COOKIE, "", 0, true, secure)
            .parse()
            .expect("cookie header"),
    );
    cookies.append(
        header::SET_COOKIE,
        set_cookie(CSRF_COOKIE, "", 0, false, secure)
            .parse()
            .expect("cookie header"),
    );
    (StatusCode::OK, cookies, Json(json!({ "ok": true }))).into_response()
}

pub async fn me(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let user = st.require_user(&headers).await?;
    let principal = st.principal(&headers).await;
    let totp_enabled = st
        .store
        .get_user(&user)
        .await
        .ok()
        .flatten()
        .map(|u| !u.totp_secret.is_empty())
        .unwrap_or(false);
    Ok(Json(json!({
        "user": user,
        "readonly": st.cfg.readonly,
        "clusters": st.cluster_ids().await,
        // 不只看 env：自动发现出来的 Prometheus 也算"已启用"
        "prometheus": st.prometheus_base().await.is_ok(),
        // 多租户：角色 + 命名空间白名单（admin 忽略白名单）
        "role": principal.as_ref().map(|p| p.role.clone()).unwrap_or_else(|| "admin".into()),
        "allowNs": principal.map(|p| p.allow_ns).unwrap_or_default(),
        "totpEnabled": totp_enabled,
        "cookieSecure": st.cfg.cookie_secure,
    })))
}

// ---------------------------------------------------------------- 改密码

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordChange {
    pub old_password: String,
    pub new_password: String,
}

fn password_problem(pwd: &str) -> Option<String> {
    if pwd.chars().count() < 8 {
        return Some("新密码至少 8 位".into());
    }
    if pwd.chars().all(|c| c.is_ascii_digit()) {
        return Some("新密码不能全是数字".into());
    }
    None
}

/// POST /api/me/password —— 自助改密码（本地账号）。
pub async fn change_password(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<PasswordChange>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = st.require_user(&headers).await?;
    if let Some(problem) = password_problem(&body.new_password) {
        return Err(ApiError(Error::BadRequest(problem)));
    }
    let Some(record) = st.store.get_user(&user).await? else {
        return Err(ApiError(Error::BadRequest(
            "内置管理员（环境变量）不支持在这里改密码，请改 FLINKOPS_ADMIN_PASSWORD 后重启".into(),
        )));
    };
    if !flinkops_store::verify_password(&body.old_password, &record.password_hash) {
        st.audit(
            &user,
            "auth.password",
            &user,
            "",
            "",
            "denied",
            Some("旧密码不对".into()),
            None,
        );
        return Err(ApiError(Error::BadRequest("旧密码不正确".into())));
    }
    if body.old_password == body.new_password {
        return Err(ApiError(Error::BadRequest("新密码不能与旧密码相同".into())));
    }
    let hash = flinkops_store::hash_password(&body.new_password);
    st.store.set_password_hash(&user, &hash).await?;
    st.audit(&user, "auth.password", &user, "", "", "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 两步验证

fn require_local_user(
    user: &str,
    record: Option<flinkops_store::UserRecord>,
) -> ApiResult<flinkops_store::UserRecord> {
    record.ok_or_else(|| {
        ApiError(Error::BadRequest(format!(
            "{user} 是环境变量里的内置管理员，不支持两步验证；请先在「成员与权限」里建本地账号"
        )))
    })
}

/// POST /api/me/2fa/setup —— 生成待确认密钥（还不生效）。
pub async fn totp_setup(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let user = st.require_user(&headers).await?;
    let record = require_local_user(&user, st.store.get_user(&user).await?)?;
    if !record.totp_secret.is_empty() {
        return Err(ApiError(Error::Conflict("两步验证已开启".into())));
    }
    let secret = crate::totp::generate_secret();
    st.store.set_totp_pending(&user, &secret).await?;
    Ok(Json(json!({
        "ok": true,
        "secret": secret,
        "otpauth": crate::totp::otpauth_uri("FlinkOps", &user, &secret),
        "note": "用验证器 App 添加后，输入 6 位动态码点「启用」才会生效",
    })))
}

#[derive(Deserialize)]
pub struct TotpCode {
    pub code: String,
}

/// POST /api/me/2fa/enable —— 用当前动态码确认并启用。
pub async fn totp_enable(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TotpCode>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = st.require_user(&headers).await?;
    let record = require_local_user(&user, st.store.get_user(&user).await?)?;
    if record.totp_pending.is_empty() {
        return Err(ApiError(Error::BadRequest(
            "先调用 /api/me/2fa/setup 生成密钥".into(),
        )));
    }
    if !crate::totp::verify(&record.totp_pending, &body.code) {
        return Err(ApiError(Error::BadRequest("验证码不正确".into())));
    }
    st.store.enable_totp(&user).await?;
    st.audit(
        &user,
        "auth.2fa",
        &user,
        "",
        "",
        "ok",
        Some("enabled".into()),
        None,
    );
    Ok(Json(json!({ "ok": true, "totpEnabled": true })))
}

/// POST /api/me/2fa/disable —— 用当前动态码关闭（防"手机丢了"也该走管理员重置）。
pub async fn totp_disable(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TotpCode>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = st.require_user(&headers).await?;
    let record = require_local_user(&user, st.store.get_user(&user).await?)?;
    if record.totp_secret.is_empty() {
        return Ok(Json(json!({ "ok": true, "totpEnabled": false })));
    }
    if !crate::totp::verify(&record.totp_secret, &body.code) {
        return Err(ApiError(Error::BadRequest("验证码不正确".into())));
    }
    st.store.disable_totp(&user).await?;
    st.audit(
        &user,
        "auth.2fa",
        &user,
        "",
        "",
        "ok",
        Some("disabled".into()),
        None,
    );
    Ok(Json(json!({ "ok": true, "totpEnabled": false })))
}
