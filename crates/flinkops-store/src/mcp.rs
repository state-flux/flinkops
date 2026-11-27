//! MCP 机器凭据与一次性审批。

#![allow(unused_imports)]
use super::*;

impl Store {
    // 参数多是有意的：token 的每个字段都要落库，拆成 struct 反而更难读
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_mcp_token(
        &self,
        id: &str,
        name: &str,
        secret_hash: &str,
        scopes: &[String],
        allow_ns: &[String],
        allow_jobs: &[String],
        qps: i64,
        expires_at: Option<&str>,
    ) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        sqlx::query(&self.sql(
            "INSERT INTO flinkops_mcp_tokens
             (id, name, secret_hash, scopes, allow_ns, allow_jobs, qps, expires_at, created_at)
             VALUES (?,?,?,?,?,?,?,?,?)",
        ))
        .bind(id)
        .bind(name)
        .bind(secret_hash)
        .bind(serde_json::to_string(scopes).unwrap_or_else(|_| "[]".into()))
        .bind(serde_json::to_string(allow_ns).unwrap_or_else(|_| "[]".into()))
        .bind(serde_json::to_string(allow_jobs).unwrap_or_else(|_| "[]".into()))
        .bind(qps)
        .bind(expires_at)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("insert mcp token: {e}")))?;
        Ok(())
    }

    fn map_token(row: &sqlx::any::AnyRow) -> Option<McpToken> {
        let parse = |k: &str| -> Vec<String> {
            row.try_get::<String, _>(k)
                .ok()
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default()
        };
        Some(McpToken {
            id: row.try_get("id").ok()?,
            name: row.try_get("name").ok()?,
            scopes: parse("scopes"),
            allow_ns: parse("allow_ns"),
            allow_jobs: parse("allow_jobs"),
            qps: row.try_get::<i64, _>("qps").unwrap_or(5),
            expires_at: row.try_get("expires_at").ok(),
            created_at: row.try_get("created_at").ok()?,
            last_used_at: row.try_get("last_used_at").ok(),
            revoked_at: row.try_get("revoked_at").ok(),
        })
    }

    pub async fn list_mcp_tokens(&self) -> Result<Vec<McpToken>> {
        let rows =
            sqlx::query(&self.sql("SELECT * FROM flinkops_mcp_tokens ORDER BY created_at DESC"))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("list mcp tokens: {e}")))?;
        Ok(rows.iter().filter_map(Self::map_token).collect())
    }

    pub async fn get_mcp_token(&self, id: &str) -> Result<Option<McpToken>> {
        let row = sqlx::query(&self.sql("SELECT * FROM flinkops_mcp_tokens WHERE id=?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get mcp token: {e}")))?;
        Ok(row.as_ref().and_then(Self::map_token))
    }

    pub async fn mcp_token_secret_hash(&self, id: &str) -> Result<Option<String>> {
        let row = sqlx::query(&self.sql("SELECT secret_hash FROM flinkops_mcp_tokens WHERE id=?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get mcp secret: {e}")))?;
        Ok(row.and_then(|r| r.try_get::<String, _>("secret_hash").ok()))
    }

    pub async fn touch_mcp_token(&self, id: &str) {
        let now = flinkops_types::now_rfc3339();
        let _ = sqlx::query(&self.sql("UPDATE flinkops_mcp_tokens SET last_used_at=? WHERE id=?"))
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await;
    }

    pub async fn revoke_mcp_token(&self, id: &str) -> Result<bool> {
        let now = flinkops_types::now_rfc3339();
        let r =
            sqlx::query(&self.sql(
                "UPDATE flinkops_mcp_tokens SET revoked_at=? WHERE id=? AND revoked_at IS NULL",
            ))
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("revoke mcp token: {e}")))?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn insert_mcp_approval(
        &self,
        id: &str,
        tool: &str,
        args_hash: &str,
        target: &str,
        actor: &str,
        token_id: &str,
    ) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        sqlx::query(&self.sql(
            "INSERT INTO flinkops_mcp_approvals
             (id, tool, args_hash, target, actor, token_id, status, created_at)
             VALUES (?,?,?,?,?,?,?,?)",
        ))
        .bind(id)
        .bind(tool)
        .bind(args_hash)
        .bind(target)
        .bind(actor)
        .bind(token_id)
        .bind("pending")
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("insert mcp approval: {e}")))?;
        Ok(())
    }

    fn map_approval(row: &sqlx::any::AnyRow) -> Option<McpApproval> {
        Some(McpApproval {
            id: row.try_get("id").ok()?,
            tool: row.try_get("tool").ok()?,
            args_hash: row.try_get("args_hash").ok()?,
            target: row.try_get("target").ok()?,
            actor: row.try_get("actor").ok()?,
            token_id: row.try_get("token_id").ok()?,
            status: row.try_get("status").ok()?,
            created_at: row.try_get("created_at").ok()?,
            decided_at: row.try_get("decided_at").ok(),
            decided_by: row.try_get("decided_by").ok(),
        })
    }

    pub async fn list_mcp_approvals(&self, status: Option<&str>) -> Result<Vec<McpApproval>> {
        let rows =
            match status {
                Some(s) => sqlx::query(&self.sql(
                    "SELECT * FROM flinkops_mcp_approvals WHERE status=? ORDER BY created_at DESC",
                ))
                .bind(s)
                .fetch_all(&self.pool)
                .await,
                None => {
                    sqlx::query(
                        &self.sql("SELECT * FROM flinkops_mcp_approvals ORDER BY created_at DESC"),
                    )
                    .fetch_all(&self.pool)
                    .await
                }
            }
            .map_err(|e| Error::Internal(format!("list mcp approvals: {e}")))?;
        Ok(rows.iter().filter_map(Self::map_approval).collect())
    }

    pub async fn get_mcp_approval(&self, id: &str) -> Result<Option<McpApproval>> {
        let row = sqlx::query(&self.sql("SELECT * FROM flinkops_mcp_approvals WHERE id=?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get mcp approval: {e}")))?;
        Ok(row.as_ref().and_then(Self::map_approval))
    }

    /// 批准：写入一次性 token 的 hash，状态 pending → approved。
    pub async fn approve_mcp_approval(&self, id: &str, by: &str, token_hash: &str) -> Result<bool> {
        let now = flinkops_types::now_rfc3339();
        let r = sqlx::query(&self.sql("UPDATE flinkops_mcp_approvals SET status='approved', decided_at=?, decided_by=?, token_hash=?
             WHERE id=? AND status='pending'"),
        )
        .bind(&now)
        .bind(by)
        .bind(token_hash)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("approve mcp approval: {e}")))?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn reject_mcp_approval(&self, id: &str, by: &str) -> Result<bool> {
        let now = flinkops_types::now_rfc3339();
        let r = sqlx::query(&self.sql(
            "UPDATE flinkops_mcp_approvals SET status='rejected', decided_at=?, decided_by=?
             WHERE id=? AND status='pending'",
        ))
        .bind(&now)
        .bind(by)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("reject mcp approval: {e}")))?;
        Ok(r.rows_affected() > 0)
    }

    /// 核销一次性 token：approved → used，并回传它绑定的审批行。
    pub async fn consume_mcp_approval(&self, token_hash: &str) -> Result<Option<McpApproval>> {
        let now = flinkops_types::now_rfc3339();
        let row =
            sqlx::query(&self.sql(
                "SELECT * FROM flinkops_mcp_approvals WHERE token_hash=? AND status='approved'",
            ))
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("find mcp approval: {e}")))?;
        let Some(row) = row else { return Ok(None) };
        let approval = Self::map_approval(&row);
        sqlx::query(
            &self.sql("UPDATE flinkops_mcp_approvals SET status='used', decided_at=? WHERE id=?"),
        )
        .bind(&now)
        .bind(row.try_get::<String, _>("id").unwrap_or_default())
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("consume mcp approval: {e}")))?;
        Ok(approval)
    }
}
