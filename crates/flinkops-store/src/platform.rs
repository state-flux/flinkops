//! 平台自身：集群接入信息、KV 设置、SQLite 维护（VACUUM/备份体积）。

#![allow(unused_imports)]
use super::*;

impl Store {
    /// 动态接入的集群（env 里的不算，env 优先）。
    pub async fn list_clusters(&self) -> Result<Vec<(String, String, Option<String>, bool)>> {
        let rows = sqlx::query(&self.sql("SELECT * FROM flinkops_clusters ORDER BY created_at"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list clusters: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("id").unwrap_or_default(),
                    r.try_get::<String, _>("source_kind")
                        .unwrap_or_else(|_| "incluster".into()),
                    r.try_get::<Option<String>, _>("kubeconfig_path")
                        .ok()
                        .flatten(),
                    r.try_get::<i64, _>("readonly").unwrap_or(0) == 1,
                )
            })
            .collect())
    }

    pub async fn save_cluster(
        &self,
        id: &str,
        kind: &str,
        path: Option<&str>,
        readonly: bool,
    ) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let updated = sqlx::query(&self.sql(
            "UPDATE flinkops_clusters SET source_kind=?, kubeconfig_path=?, readonly=? WHERE id=?",
        ))
        .bind(kind)
        .bind(path)
        .bind(if readonly { 1i64 } else { 0 })
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("update cluster: {e}")))?;
        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_clusters (id, source_kind, kubeconfig_path, readonly, created_at) VALUES (?,?,?,?,?)"))
                .bind(id).bind(kind).bind(path).bind(if readonly { 1i64 } else { 0 }).bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert cluster: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_cluster(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_clusters WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete cluster: {e}")))?;
        Ok(())
    }

    // ---------------------------------------------------------- settings

    pub async fn get_settings(&self) -> Result<std::collections::HashMap<String, String>> {
        let rows = sqlx::query(&self.sql("SELECT k, v FROM flinkops_settings"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get settings: {e}")))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let k: String = r.try_get("k").ok()?;
                let v: String = r.try_get("v").ok()?;
                Some((k, v))
            })
            .collect())
    }

    pub async fn get_setting(&self, key: &str) -> Option<String> {
        let row = sqlx::query(&self.sql("SELECT v FROM flinkops_settings WHERE k = ?"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .ok()??;
        row.try_get::<String, _>("v").ok()
    }

    pub async fn set_settings(&self, kv: &std::collections::HashMap<String, String>) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        for (k, v) in kv {
            let updated = sqlx::query(
                &self.sql("UPDATE flinkops_settings SET v = ?, updated_at = ? WHERE k = ?"),
            )
            .bind(v)
            .bind(&now)
            .bind(k)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("update setting: {e}")))?;
            if updated.rows_affected() == 0 {
                sqlx::query(
                    &self.sql("INSERT INTO flinkops_settings (k, v, updated_at) VALUES (?,?,?)"),
                )
                .bind(k)
                .bind(v)
                .bind(&now)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("insert setting: {e}")))?;
            }
        }
        Ok(())
    }

    pub async fn delete_settings(&self) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_settings"))
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("reset settings: {e}")))?;
        Ok(())
    }

    // ---------------------------------------------------------- alerts

    /// SQLite VACUUM（其他后端直接跳过：PG/MySQL 有自己的维护机制）。
    pub async fn vacuum(&self) -> Result<bool> {
        // 只有 SQLite 需要 VACUUM；PG/MySQL 有 autovacuum / 引擎自身回收
        if !self.dialect().is_sqlite() {
            return Ok(false);
        }
        let is_sqlite = self
            .dsn_hint
            .as_deref()
            .map(|d| d.starts_with("sqlite"))
            .unwrap_or(false);
        if !is_sqlite {
            return Ok(false);
        }
        sqlx::query(&self.sql("VACUUM"))
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("vacuum: {e}")))?;
        Ok(true)
    }

    /// 备份信息：SQLite 返回文件路径与大小；远程后端返回 None。
    pub fn backup_info(&self) -> Option<(String, u64)> {
        // 文件级备份信息只有 SQLite 有意义；PG/MySQL 交给 pg_dump / mysqldump
        if !self.dialect().is_sqlite() {
            return None;
        }
        let dsn = self.dsn_hint.as_deref()?;
        let path = sqlite_path(dsn)?.to_string();
        let size = std::fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0);
        Some((path, size))
    }

    /// SQLite 文件大小（其他后端返回 None，由外部监控 DB 指标）。
    pub fn db_bytes(&self) -> Option<u64> {
        if !self.dialect().is_sqlite() {
            return None;
        }
        let path = sqlite_path(self.dsn_hint.as_deref()?)?;
        std::fs::metadata(path).ok().map(|m| m.len())
    }
}
