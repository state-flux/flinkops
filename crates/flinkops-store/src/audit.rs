//! 审计：写入、分页/过滤查询、保留期清理。

#![allow(unused_imports)]
use super::*;

impl Store {
    pub async fn audit(&self, e: &AuditEntry) -> Result<()> {
        sqlx::query(&self.sql("INSERT INTO flinkops_audit (id, ts, actor, action, target, cluster, ns, result, detail, ip)
             VALUES (?,?,?,?,?,?,?,?,?,?)"),
        )
        .bind(&e.id)
        .bind(&e.ts)
        .bind(&e.actor)
        .bind(&e.action)
        .bind(&e.target)
        .bind(&e.cluster)
        .bind(&e.namespace)
        .bind(&e.result)
        .bind(&e.detail)
        .bind(&e.ip)
        .execute(&self.pool)
        .await
        .map_err(|err| Error::Internal(format!("audit: {err}")))?;
        Ok(())
    }

    pub async fn list_audit(&self, limit: i64) -> Result<Vec<AuditEntry>> {
        let rows = sqlx::query(&self.sql("SELECT * FROM flinkops_audit ORDER BY ts DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list audit: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| AuditEntry {
                id: r.try_get("id").unwrap_or_default(),
                ts: r.try_get("ts").unwrap_or_default(),
                actor: r.try_get("actor").unwrap_or_default(),
                action: r.try_get("action").unwrap_or_default(),
                target: r.try_get("target").unwrap_or_default(),
                cluster: r.try_get("cluster").unwrap_or_default(),
                namespace: r.try_get("ns").unwrap_or_default(),
                result: r.try_get("result").unwrap_or_default(),
                detail: crate::text_col_opt(&r, "detail"),
                ip: r.try_get("ip").ok(),
            })
            .collect())
    }

    /// 带过滤条件的审计查询（分页）。
    ///
    /// 审计是排障与合规的第一现场，只给"最近 200 条"不够用：这里支持
    /// action/actor/result/namespace/关键字/时间下界，并返回总数用于分页。
    pub async fn list_audit_filtered(&self, f: &AuditFilter) -> Result<(Vec<AuditEntry>, i64)> {
        let mut where_sql = String::from(" WHERE 1=1");
        let mut binds: Vec<String> = Vec::new();
        let action = f.action.as_deref();
        let actor = f.actor.as_deref();
        let result = f.result.as_deref();
        let namespace = f.namespace.as_deref();
        let q = f.keyword.as_deref();
        let since = f.since.as_deref();
        let limit = f.limit;
        let offset = f.offset;
        if let Some(v) = action.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND action = ?");
            binds.push(v.to_string());
        }
        if let Some(v) = actor.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND actor = ?");
            binds.push(v.to_string());
        }
        if let Some(v) = result.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND result = ?");
            binds.push(v.to_string());
        }
        if let Some(v) = namespace.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND ns = ?");
            binds.push(v.to_string());
        }
        if let Some(v) = since.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND ts >= ?");
            binds.push(v.to_string());
        }
        if let Some(v) = q.filter(|s| !s.is_empty()) {
            where_sql.push_str(" AND (target LIKE ? OR actor LIKE ? OR detail LIKE ?)");
            let like = format!("%{v}%");
            binds.push(like.clone());
            binds.push(like.clone());
            binds.push(like);
        }

        let limit = limit.clamp(1, 5000);
        let offset = offset.max(0);

        let count_sql = format!("SELECT COUNT(*) FROM flinkops_audit{where_sql}");
        let count_sql = self.sql(&count_sql);
        let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
        for b in &binds {
            count_q = count_q.bind(b);
        }
        let total = count_q
            .fetch_one(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("count audit: {e}")))?;

        let list_sql =
            format!("SELECT * FROM flinkops_audit{where_sql} ORDER BY ts DESC LIMIT ? OFFSET ?");
        let list_sql = self.sql(&list_sql);
        let mut list_q = sqlx::query(&list_sql);
        for b in &binds {
            list_q = list_q.bind(b);
        }
        let rows = list_q
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list audit: {e}")))?;

        Ok((
            rows.into_iter()
                .map(|r| AuditEntry {
                    id: r.try_get("id").unwrap_or_default(),
                    ts: r.try_get("ts").unwrap_or_default(),
                    actor: r.try_get("actor").unwrap_or_default(),
                    action: r.try_get("action").unwrap_or_default(),
                    target: r.try_get("target").unwrap_or_default(),
                    cluster: r.try_get("cluster").unwrap_or_default(),
                    namespace: r.try_get("ns").unwrap_or_default(),
                    result: r.try_get("result").unwrap_or_default(),
                    detail: crate::text_col_opt(&r, "detail"),
                    ip: r.try_get("ip").ok(),
                })
                .collect(),
            total,
        ))
    }

    /// 保留期清理：返回删掉的行数。`before` 是 RFC3339（ISO 字典序可比较）。
    pub async fn purge_audit(&self, before: &str) -> Result<u64> {
        let res = sqlx::query(&self.sql("DELETE FROM flinkops_audit WHERE ts < ?"))
            .bind(before)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("purge audit: {e}")))?;
        Ok(res.rows_affected())
    }

    // ---------------------------------------------------------- 部署模板
}
