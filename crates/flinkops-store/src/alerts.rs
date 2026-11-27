//! 告警规则与事件。

#![allow(unused_imports)]
use super::*;

impl Store {
    pub async fn list_rules(&self) -> Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(&self.sql("SELECT * FROM flinkops_alert_rules ORDER BY created_at"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list rules: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "threshold": r.try_get::<Option<i64>, _>("threshold").ok().flatten(),
                    "severity": r.try_get::<String, _>("severity").unwrap_or_default(),
                    "enabled": r.try_get::<i64, _>("enabled").unwrap_or(1) == 1,
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn save_rule(&self, rule: &serde_json::Value) -> Result<()> {
        let id = rule
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let name = rule
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let kind = rule
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let threshold = rule.get("threshold").and_then(|v| v.as_i64());
        let severity = rule
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("warn");
        let enabled = rule
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let now = flinkops_types::now_rfc3339();

        let updated = sqlx::query(&self.sql("UPDATE flinkops_alert_rules SET name=?, kind=?, threshold=?, severity=?, enabled=? WHERE id=?"),
        )
        .bind(name).bind(kind).bind(threshold).bind(severity).bind(if enabled { 1i64 } else { 0 })
        .bind(&id)
        .execute(&self.pool).await
        .map_err(|e| Error::Internal(format!("update rule: {e}")))?;

        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_alert_rules (id, name, kind, threshold, severity, enabled, created_at) VALUES (?,?,?,?,?,?,?)"))
                .bind(&id).bind(name).bind(kind).bind(threshold).bind(severity)
                .bind(if enabled { 1i64 } else { 0 }).bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert rule: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_rule(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_alert_rules WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete rule: {e}")))?;
        Ok(())
    }

    pub async fn list_events(&self, state: Option<&str>) -> Result<Vec<serde_json::Value>> {
        let rows = match state {
            Some(s) => sqlx::query(&self.sql(
                "SELECT * FROM flinkops_alert_events WHERE state = ? ORDER BY ts DESC LIMIT 200",
            ))
            .bind(s)
            .fetch_all(&self.pool)
            .await,
            None => {
                sqlx::query(
                    &self.sql("SELECT * FROM flinkops_alert_events ORDER BY ts DESC LIMIT 200"),
                )
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| Error::Internal(format!("list events: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "ruleId": r.try_get::<String, _>("rule_id").unwrap_or_default(),
                    "ruleName": r.try_get::<String, _>("rule_name").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "cluster": r.try_get::<String, _>("cluster").unwrap_or_default(),
                    "namespace": r.try_get::<String, _>("ns").unwrap_or_default(),
                    "job": r.try_get::<String, _>("job").unwrap_or_default(),
                    "severity": r.try_get::<String, _>("severity").unwrap_or_default(),
                    "message": r.try_get::<String, _>("message").unwrap_or_default(),
                    "state": r.try_get::<String, _>("state").unwrap_or_default(),
                    "ts": r.try_get::<String, _>("ts").unwrap_or_default(),
                })
            })
            .collect())
    }

    /// 去重写入：同一 rule+job 已 OPEN 就刷新时间，否则插入新事件。
    pub async fn upsert_event(&self, e: &serde_json::Value) -> Result<()> {
        let rule_id = e.get("ruleId").and_then(|v| v.as_str()).unwrap_or_default();
        let job = e.get("job").and_then(|v| v.as_str()).unwrap_or_default();
        let now = flinkops_types::now_rfc3339();

        let existing = sqlx::query(&self.sql(
            "SELECT id FROM flinkops_alert_events WHERE rule_id = ? AND job = ? AND state = 'OPEN'",
        ))
        .bind(rule_id)
        .bind(job)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| Error::Internal(format!("lookup event: {err}")))?;

        if let Some(row) = existing {
            let id: String = row.try_get("id").unwrap_or_default();
            sqlx::query(&self.sql("UPDATE flinkops_alert_events SET ts=?, message=? WHERE id=?"))
                .bind(&now)
                .bind(
                    e.get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default(),
                )
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|err| Error::Internal(format!("refresh event: {err}")))?;
            return Ok(());
        }

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(&self.sql("INSERT INTO flinkops_alert_events (id, rule_id, rule_name, kind, cluster, ns, job, severity, message, state, ts) VALUES (?,?,?,?,?,?,?,?,?,?,?)"))
            .bind(&id)
            .bind(rule_id)
            .bind(e.get("ruleName").and_then(|v| v.as_str()).unwrap_or_default())
            .bind(e.get("kind").and_then(|v| v.as_str()).unwrap_or_default())
            .bind(e.get("cluster").and_then(|v| v.as_str()).unwrap_or_default())
            .bind(e.get("namespace").and_then(|v| v.as_str()).unwrap_or_default())
            .bind(job)
            .bind(e.get("severity").and_then(|v| v.as_str()).unwrap_or("warn"))
            .bind(e.get("message").and_then(|v| v.as_str()).unwrap_or_default())
            .bind("OPEN")
            .bind(&now)
            .execute(&self.pool).await
            .map_err(|err| Error::Internal(format!("insert event: {err}")))?;
        Ok(())
    }

    /// 关闭某规则本轮**没有命中**的 OPEN 事件（告警自愈）。
    ///
    /// 之前只有 nodeport_exposed 会关事件，作业恢复后 `job_not_running` 之类的
    /// OPEN 记录会永远挂着，首页活跃告警越积越多。
    pub async fn close_stale_events(
        &self,
        rule_id: &str,
        hits: &[(String, String)],
    ) -> Result<usize> {
        let now = flinkops_types::now_rfc3339();
        let rows = sqlx::query(&self.sql("SELECT id, cluster, job FROM flinkops_alert_events WHERE rule_id = ? AND state = 'OPEN'"),
        )
        .bind(rule_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("list open events: {e}")))?;
        let keep: std::collections::HashSet<(String, String)> = hits.iter().cloned().collect();
        let mut n = 0usize;
        for row in rows {
            let id: String = row.try_get("id").unwrap_or_default();
            let cluster: String = row.try_get("cluster").unwrap_or_default();
            let job: String = row.try_get("job").unwrap_or_default();
            if keep.contains(&(cluster.clone(), job.clone())) {
                continue;
            }
            sqlx::query(
                &self.sql("UPDATE flinkops_alert_events SET state='CLOSED', ts=? WHERE id=?"),
            )
            .bind(&now)
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("close event: {e}")))?;
            n += 1;
        }
        Ok(n)
    }

    /// 关闭某个规则在某集群下的全部 OPEN 事件（规则不再适用时用，例如平台改成
    /// in-cluster 后不再建议改 ClusterIP）。
    pub async fn close_events(&self, rule_id: &str, cluster: Option<&str>) -> Result<usize> {
        let now = flinkops_types::now_rfc3339();
        let r = match cluster {
            Some(c) => {
                sqlx::query(&self.sql("UPDATE flinkops_alert_events SET state='CLOSED', ts=? WHERE rule_id=? AND cluster=? AND state='OPEN'"),
                )
                .bind(&now)
                .bind(rule_id)
                .bind(c)
                .execute(&self.pool)
                .await
            }
            None => {
                sqlx::query(&self.sql("UPDATE flinkops_alert_events SET state='CLOSED', ts=? WHERE rule_id=? AND state='OPEN'"),
                )
                .bind(&now)
                .bind(rule_id)
                .execute(&self.pool)
                .await
            }
        }
        .map_err(|e| Error::Internal(format!("close events: {e}")))?;
        Ok(r.rows_affected() as usize)
    }

    pub async fn ack_event(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("UPDATE flinkops_alert_events SET state = 'ACK' WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("ack event: {e}")))?;
        Ok(())
    }

    // ---------------------------------------------------------- sessions
}
