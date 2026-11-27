// 多租户账号与角色（角色与命名空间白名单的最小实现）。
//
// 密码存储：**argon2id**（PHC 字符串 `$argon2id$v=19$...`）。
// 历史版本用的是单轮 `sha256(salt:pwd)`；为了不把老库里的账号锁在门外，
// `verify_password` 仍能校验老格式，并在登录成功后由调用方 rehash 升级
// （见 `needs_rehash`）。
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, SaltString};
use argon2::{Argon2, PasswordVerifier};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{Error, Result, Store};

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub allow_ns: Vec<String>,
    pub disabled: bool,
    pub created_at: String,
    /// 已启用的 TOTP 密钥（空 = 未开启两步验证）
    pub totp_secret: String,
    /// 正在设置中的 TOTP 密钥（还没用验证码确认）
    pub totp_pending: String,
}

/// 新密码一律 argon2id。hash 失败（几乎不可能）时退回 sha256，保证"能登录"优先。
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    match Argon2::default().hash_password(password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "argon2 hash failed, falling back to legacy sha256");
            legacy_hash(password)
        }
    }
}

/// 校验密码：PHC 串走 argon2；`salt$hex` 走老的 sha256。
pub fn verify_password(password: &str, stored: &str) -> bool {
    if stored.starts_with("$argon2") {
        return PasswordHash::new(stored)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false);
    }
    legacy_verify(password, stored)
}

/// 存的是老格式（sha256）→ 登录成功后应重新 hash 并落库。
pub fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

fn legacy_hash(password: &str) -> String {
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let digest = Sha256::digest(format!("{salt}:{password}").as_bytes());
    format!("{salt}${digest:x}")
}

fn legacy_verify(password: &str, stored: &str) -> bool {
    let Some((salt, hash)) = stored.split_once('$') else {
        return false;
    };
    let digest = Sha256::digest(format!("{salt}:{password}").as_bytes());
    // 常量时间比较，避免时序侧信道
    let expected = format!("{digest:x}");
    expected.len() == hash.len()
        && expected
            .bytes()
            .zip(hash.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn parse_ns(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

const USER_COLUMNS: &str =
    "username, password_hash, role, allow_ns, disabled, created_at, totp_secret, totp_pending";

fn to_record(r: &sqlx::any::AnyRow) -> UserRecord {
    UserRecord {
        username: r.try_get("username").unwrap_or_default(),
        password_hash: r.try_get("password_hash").unwrap_or_default(),
        role: r.try_get("role").unwrap_or_else(|_| "viewer".into()),
        allow_ns: parse_ns(&r.try_get::<String, _>("allow_ns").unwrap_or_default()),
        disabled: r.try_get::<i64, _>("disabled").unwrap_or(0) != 0,
        created_at: r.try_get("created_at").unwrap_or_default(),
        // 老库升级上来时这两列可能还是 NULL
        totp_secret: r.try_get("totp_secret").unwrap_or_default(),
        totp_pending: r.try_get("totp_pending").unwrap_or_default(),
    }
}

impl Store {
    pub async fn get_user(&self, username: &str) -> Result<Option<UserRecord>> {
        let raw = format!("SELECT {USER_COLUMNS} FROM flinkops_users WHERE username = ?");
        let sql = self.sql(&raw);
        let row = sqlx::query(&sql)
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get user: {e}")))?;
        Ok(row.as_ref().map(to_record))
    }

    pub async fn list_users(&self) -> Result<Vec<UserRecord>> {
        let raw = format!("SELECT {USER_COLUMNS} FROM flinkops_users ORDER BY role, username");
        let sql = self.sql(&raw);
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list users: {e}")))?;
        Ok(rows.iter().map(to_record).collect())
    }

    pub async fn upsert_user(
        &self,
        username: &str,
        password_hash: Option<&str>,
        role: &str,
        allow_ns: &[String],
    ) -> Result<()> {
        let ns = serde_json::to_string(allow_ns).unwrap_or_else(|_| "[]".into());
        let updated = sqlx::query(
            &self.sql("UPDATE flinkops_users SET role = ?, allow_ns = ? WHERE username = ?"),
        )
        .bind(role)
        .bind(&ns)
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("update user: {e}")))?;
        if updated.rows_affected() > 0 {
            if let Some(h) = password_hash {
                sqlx::query(
                    &self.sql("UPDATE flinkops_users SET password_hash = ? WHERE username = ?"),
                )
                .bind(h)
                .bind(username)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("update password: {e}")))?;
            }
            return Ok(());
        }
        let hash = password_hash.unwrap_or_default();
        sqlx::query(&self.sql("INSERT INTO flinkops_users (username, password_hash, role, allow_ns, disabled, created_at)
             VALUES (?,?,?,?,0,?)"),
        )
        .bind(username)
        .bind(hash)
        .bind(role)
        .bind(&ns)
        .bind(flinkops_types::now_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("insert user: {e}")))?;
        Ok(())
    }

    /// 改密码 / 老 hash 升级到 argon2 都走这里。
    pub async fn set_password_hash(&self, username: &str, hash: &str) -> Result<u64> {
        let r = sqlx::query(
            &self.sql("UPDATE flinkops_users SET password_hash = ? WHERE username = ?"),
        )
        .bind(hash)
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("set password: {e}")))?;
        Ok(r.rows_affected())
    }

    // ---------------------------------------------------------------- 2FA（TOTP）

    /// 生成待确认密钥（还不生效，登录不校验）。
    pub async fn set_totp_pending(&self, username: &str, secret: &str) -> Result<()> {
        sqlx::query(&self.sql("UPDATE flinkops_users SET totp_pending = ? WHERE username = ?"))
            .bind(secret)
            .bind(username)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("set totp pending: {e}")))?;
        Ok(())
    }

    /// 用户输入验证码确认后，pending → 正式密钥并清空 pending。
    pub async fn enable_totp(&self, username: &str) -> Result<()> {
        sqlx::query(&self.sql("UPDATE flinkops_users SET totp_secret = totp_pending, totp_pending = '' WHERE username = ?"),
        )
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("enable totp: {e}")))?;
        Ok(())
    }

    pub async fn disable_totp(&self, username: &str) -> Result<()> {
        sqlx::query(&self.sql(
            "UPDATE flinkops_users SET totp_secret = '', totp_pending = '' WHERE username = ?",
        ))
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("disable totp: {e}")))?;
        Ok(())
    }

    pub async fn delete_user(&self, username: &str) -> Result<u64> {
        let r = sqlx::query(&self.sql("DELETE FROM flinkops_users WHERE username = ?"))
            .bind(username)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete user: {e}")))?;
        Ok(r.rows_affected())
    }

    /// 还有几个未禁用的 admin（用于阻止"删掉最后一个管理员"）。
    pub async fn count_admins(&self) -> Result<i64> {
        let n: i64 = sqlx::query_scalar(
            &self.sql("SELECT COUNT(*) FROM flinkops_users WHERE role = 'admin' AND disabled = 0"),
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("count admins: {e}")))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_roundtrip_and_rehash_flag() {
        let h = hash_password("s3cret-password");
        assert!(h.starts_with("$argon2id$"), "应该是 argon2id PHC 串: {h}");
        assert!(verify_password("s3cret-password", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!needs_rehash(&h));
    }

    #[test]
    fn legacy_sha256_still_verifies_and_is_flagged_for_rehash() {
        let legacy = legacy_hash("old-password");
        assert!(!legacy.starts_with("$argon2"));
        assert!(verify_password("old-password", &legacy));
        assert!(!verify_password("nope", &legacy));
        assert!(needs_rehash(&legacy), "老格式应触发 rehash");
    }

    #[test]
    fn empty_or_garbage_hash_never_matches() {
        assert!(!verify_password("", ""));
        assert!(!verify_password("x", "not-a-hash"));
        assert!(!verify_password("x", "$argon2id$broken"));
    }
}
