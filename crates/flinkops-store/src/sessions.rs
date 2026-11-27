//! 登录会话（DB 侧）：登录不丢、过期清理。

#![allow(unused_imports)]
use super::*;

impl Store {
    /// 登录落库：重启不掉登录，多副本可共享（SQLite 单机 / PG·MySQL 共享）。
    pub async fn create_session(&self, token: &str, username: &str, ttl_hours: i64) -> Result<()> {
        let now = chrono::Utc::now();
        let created = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let expires = (now + chrono::Duration::hours(ttl_hours))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        sqlx::query(&self.sql("INSERT INTO flinkops_sessions (token, username, created_at, expires_at) VALUES (?,?,?,?)"))
            .bind(token)
            .bind(username)
            .bind(created)
            .bind(expires)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("create session: {e}")))?;
        Ok(())
    }

    /// 校验会话；过期视为无效（并顺手删掉）。
    pub async fn session_user(&self, token: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            &self.sql("SELECT username, expires_at FROM flinkops_sessions WHERE token = ?"),
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("session lookup: {e}")))?;

        let Some(row) = row else { return Ok(None) };
        let username: String = row.try_get("username").unwrap_or_default();
        let expires: String = row.try_get("expires_at").unwrap_or_default();
        let now = flinkops_types::now_rfc3339();
        if expires < now {
            let _ = sqlx::query(&self.sql("DELETE FROM flinkops_sessions WHERE token = ?"))
                .bind(token)
                .execute(&self.pool)
                .await;
            return Ok(None);
        }
        Ok(Some(username))
    }

    pub async fn delete_session(&self, token: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_sessions WHERE token = ?"))
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete session: {e}")))?;
        Ok(())
    }

    /// 清理过期会话，返回删除条数。
    pub async fn purge_sessions(&self) -> Result<u64> {
        let now = flinkops_types::now_rfc3339();
        let res = sqlx::query(&self.sql("DELETE FROM flinkops_sessions WHERE expires_at < ?"))
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("purge sessions: {e}")))?;
        Ok(res.rows_affected())
    }

    // ---------------------------------------------------------- profiles
}
