//! SQL 工作台相关：数据源（catalog）、表缓存、SQL 作业与版本、交付模板。

#![allow(unused_imports)]
use super::*;

impl Store {
    pub async fn list_deploy_templates(&self) -> Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(
            &self.sql("SELECT * FROM flinkops_deploy_templates ORDER BY builtin DESC, created_at"),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("list deploy templates: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "yamlTemplate": r.try_get::<String, _>("yaml_template").unwrap_or_default(),
                    "builtin": r.try_get::<i64, _>("builtin").unwrap_or(0) == 1,
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn get_deploy_template(&self, id: &str) -> Result<Option<serde_json::Value>> {
        Ok(self
            .list_deploy_templates()
            .await?
            .into_iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id)))
    }

    pub async fn save_deploy_template(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        yaml: &str,
    ) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let updated = sqlx::query(&self.sql(
            "UPDATE flinkops_deploy_templates SET name=?, kind=?, yaml_template=? WHERE id=?",
        ))
        .bind(name)
        .bind(kind)
        .bind(yaml)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("update deploy template: {e}")))?;
        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_deploy_templates (id, name, kind, yaml_template, builtin, created_at) VALUES (?,?,?,?,?,?)"))
                .bind(id).bind(name).bind(kind).bind(yaml).bind(0i64).bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert deploy template: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_deploy_template(&self, id: &str) -> Result<()> {
        sqlx::query(
            &self.sql("DELETE FROM flinkops_deploy_templates WHERE id = ? AND builtin = 0"),
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("delete deploy template: {e}")))?;
        Ok(())
    }

    // ---------------------------------------------------------- 外部数据源元数据

    /// 连外部数据源读库表结构。
    ///
    /// 实现走 python 脚本（scripts/sync_catalog.py + pymysql）：StarRocks 不认
    /// sqlx 默认发的 `SET` 语句（"Set statement only support constant expr"），
    /// pymysql 的握手是干净的。
    pub async fn fetch_external_schema(
        dsn: &str,
    ) -> Result<Vec<(String, String, serde_json::Value)>> {
        let script = [
            "scripts/sync_catalog.py",
            "../scripts/sync_catalog.py",
            "../../scripts/sync_catalog.py",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
        .ok_or_else(|| Error::Internal("找不到 scripts/sync_catalog.py".into()))?;

        let python = if cfg!(windows) { "python" } else { "python3" };
        let out = tokio::process::Command::new(python)
            .arg(&script)
            .arg(dsn)
            .output()
            .await
            .map_err(|e| Error::Internal(format!("调用 {python} 失败：{e}")))?;

        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value =
            serde_json::from_str(stdout.trim()).map_err(|e| Error::Upstream {
                target: "datasource".into(),
                message: format!(
                    "脚本输出不是 JSON（{e}）：{}",
                    stdout.chars().take(200).collect::<String>()
                ),
            })?;

        if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            return Err(Error::Upstream {
                target: "datasource".into(),
                message: v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("未知错误")
                    .to_string(),
            });
        }

        let mut out_tables = Vec::new();
        if let Some(arr) = v.get("tables").and_then(|x| x.as_array()) {
            for t in arr {
                let db = t.get("db").and_then(|x| x.as_str()).unwrap_or_default();
                let table = t.get("table").and_then(|x| x.as_str()).unwrap_or_default();
                let cols = t.get("columns").cloned().unwrap_or(serde_json::json!([]));
                out_tables.push((db.to_string(), table.to_string(), cols));
            }
        }
        Ok(out_tables)
    }

    pub async fn list_catalogs(&self) -> Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(&self.sql("SELECT * FROM flinkops_catalogs ORDER BY created_at"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list catalogs: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    // dsn 里的密码不外传
                    "dsnMasked": mask_dsn(&r.try_get::<String, _>("dsn").unwrap_or_default()),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn save_catalog(&self, id: &str, name: &str, kind: &str, dsn: &str) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let updated =
            sqlx::query(&self.sql("UPDATE flinkops_catalogs SET name=?, kind=?, dsn=? WHERE id=?"))
                .bind(name)
                .bind(kind)
                .bind(dsn)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("update catalog: {e}")))?;
        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_catalogs (id, name, kind, dsn, created_at) VALUES (?,?,?,?,?)"))
                .bind(id).bind(name).bind(kind).bind(dsn).bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert catalog: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_catalog(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_catalogs WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete catalog: {e}")))?;
        sqlx::query(&self.sql("DELETE FROM flinkops_catalog_tables WHERE catalog_id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete catalog tables: {e}")))?;
        Ok(())
    }

    pub async fn catalog_dsn(&self, id: &str) -> Result<Option<(String, String)>> {
        let row = sqlx::query(&self.sql("SELECT name, dsn FROM flinkops_catalogs WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get catalog: {e}")))?;
        Ok(row.map(|r| {
            (
                r.try_get::<String, _>("name").unwrap_or_default(),
                r.try_get::<String, _>("dsn").unwrap_or_default(),
            )
        }))
    }

    /// 一次同步的全量替换：先删该 catalog 的旧元数据，再插入新的。
    pub async fn replace_catalog_tables(
        &self,
        catalog_id: &str,
        tables: &[(String, String, serde_json::Value)],
    ) -> Result<usize> {
        sqlx::query(&self.sql("DELETE FROM flinkops_catalog_tables WHERE catalog_id = ?"))
            .bind(catalog_id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("clear catalog tables: {e}")))?;
        let now = flinkops_types::now_rfc3339();
        for (db, table, cols) in tables {
            let id = format!("{catalog_id}:{db}.{table}");
            sqlx::query(&self.sql("INSERT INTO flinkops_catalog_tables (id, catalog_id, db_name, table_name, columns_json, synced_at) VALUES (?,?,?,?,?,?)"))
                .bind(&id).bind(catalog_id).bind(db).bind(table)
                .bind(serde_json::to_string(cols).unwrap_or_else(|_| "[]".into()))
                .bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert catalog table: {e}")))?;
        }
        Ok(tables.len())
    }

    pub async fn list_catalog_tables(&self, catalog_id: &str) -> Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(&self.sql("SELECT * FROM flinkops_catalog_tables WHERE catalog_id = ? ORDER BY db_name, table_name"))
            .bind(catalog_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list catalog tables: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let cols: String = r.try_get("columns_json").unwrap_or_else(|_| "[]".into());
                serde_json::json!({
                    "db": r.try_get::<Option<String>, _>("db_name").ok().flatten(),
                    "table": r.try_get::<Option<String>, _>("table_name").ok().flatten(),
                    "columns": serde_json::from_str::<serde_json::Value>(&cols).unwrap_or(serde_json::json!([])),
                    "syncedAt": r.try_get::<Option<String>, _>("synced_at").ok().flatten(),
                })
            })
            .collect())
    }

    // ---------------------------------------------------------- SQL jobs

    pub async fn list_sql_jobs(&self) -> Result<Vec<serde_json::Value>> {
        let rows =
            sqlx::query(&self.sql("SELECT * FROM flinkops_sql_jobs ORDER BY updated_at DESC"))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("list sql jobs: {e}")))?;
        Ok(rows.into_iter().map(sql_job_row).collect())
    }

    pub async fn get_sql_job(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let row = sqlx::query(&self.sql("SELECT * FROM flinkops_sql_jobs WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("get sql job: {e}")))?;
        Ok(row.map(sql_job_row))
    }

    pub async fn upsert_sql_job(&self, j: &serde_json::Value) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let id = j
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let g = |k: &str| j.get(k).cloned().unwrap_or(serde_json::Value::Null);
        let updated = sqlx::query(&self.sql("UPDATE flinkops_sql_jobs SET name=?, cluster=?, ns=?, session=?, sql_text=?, version=?, job_id=?, state=?, last_error=?, updated_at=? WHERE id=?"),
        )
        .bind(g("name").as_str().unwrap_or_default())
        .bind(g("cluster").as_str())
        .bind(g("namespace").as_str())
        .bind(g("session").as_str())
        .bind(g("sqlText").as_str().unwrap_or_default())
        .bind(g("version").as_i64().unwrap_or(1))
        .bind(g("jobId").as_str())
        .bind(g("state").as_str().unwrap_or("DRAFT"))
        .bind(g("lastError").as_str())
        .bind(&now)
        .bind(&id)
        .execute(&self.pool).await
        .map_err(|e| Error::Internal(format!("update sql job: {e}")))?;

        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_sql_jobs (id, name, cluster, ns, session, sql_text, version, job_id, state, last_error, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"))
                .bind(&id)
                .bind(g("name").as_str().unwrap_or_default())
                .bind(g("cluster").as_str())
                .bind(g("namespace").as_str())
                .bind(g("session").as_str())
                .bind(g("sqlText").as_str().unwrap_or_default())
                .bind(g("version").as_i64().unwrap_or(1))
                .bind(g("jobId").as_str())
                .bind(g("state").as_str().unwrap_or("DRAFT"))
                .bind(g("lastError").as_str())
                .bind(&now).bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert sql job: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_sql_job(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_sql_jobs WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete sql job: {e}")))?;
        sqlx::query(&self.sql("DELETE FROM flinkops_sql_versions WHERE sql_job_id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete sql versions: {e}")))?;
        Ok(())
    }

    pub async fn add_sql_version(
        &self,
        job_id: &str,
        version: i64,
        sql_text: &str,
        note: Option<&str>,
    ) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(&self.sql("INSERT INTO flinkops_sql_versions (id, sql_job_id, version, sql_text, note, created_at) VALUES (?,?,?,?,?,?)"))
            .bind(&id).bind(job_id).bind(version).bind(sql_text).bind(note).bind(&now)
            .execute(&self.pool).await
            .map_err(|e| Error::Internal(format!("insert sql version: {e}")))?;
        Ok(())
    }

    pub async fn list_sql_versions(&self, job_id: &str) -> Result<Vec<serde_json::Value>> {
        let rows =
            sqlx::query(&self.sql(
                "SELECT * FROM flinkops_sql_versions WHERE sql_job_id = ? ORDER BY version DESC",
            ))
            .bind(job_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list sql versions: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "version": r.try_get::<i64, _>("version").unwrap_or(0),
                    "sqlText": crate::text_col(&r, "sql_text"),
                    "note": r.try_get::<Option<String>, _>("note").ok().flatten(),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    // ---------------------------------------------------------- templates

    pub async fn list_templates(&self) -> Result<Vec<serde_json::Value>> {
        let rows =
            sqlx::query(&self.sql("SELECT * FROM flinkops_templates ORDER BY created_at DESC"))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| Error::Internal(format!("list templates: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let payload: String = crate::text_col(&r, "payload");
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "name": r.try_get::<String, _>("name").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "sourceJob": r.try_get::<Option<String>, _>("source_job").ok().flatten(),
                    "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(serde_json::json!({})),
                    "createdAt": r.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn save_template(&self, t: &serde_json::Value) -> Result<()> {
        let now = flinkops_types::now_rfc3339();
        let id = t
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let payload_value = t.get("payload").cloned().unwrap_or(serde_json::json!({}));
        let payload = serde_json::to_string(&payload_value).unwrap_or_else(|_| "{}".to_string());
        let updated = sqlx::query(&self.sql(
            "UPDATE flinkops_templates SET name=?, kind=?, source_job=?, payload=? WHERE id=?",
        ))
        .bind(t.get("name").and_then(|v| v.as_str()).unwrap_or_default())
        .bind(
            t.get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("FlinkDeployment"),
        )
        .bind(t.get("sourceJob").and_then(|v| v.as_str()))
        .bind(&payload)
        .bind(&id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("update template: {e}")))?;

        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_templates (id, name, kind, source_job, payload, created_at) VALUES (?,?,?,?,?,?)"))
                .bind(&id)
                .bind(t.get("name").and_then(|v| v.as_str()).unwrap_or_default())
                .bind(t.get("kind").and_then(|v| v.as_str()).unwrap_or("FlinkDeployment"))
                .bind(t.get("sourceJob").and_then(|v| v.as_str()))
                .bind(&payload)
                .bind(&now)
                .execute(&self.pool).await
                .map_err(|e| Error::Internal(format!("insert template: {e}")))?;
        }
        Ok(())
    }

    pub async fn delete_template(&self, id: &str) -> Result<()> {
        sqlx::query(&self.sql("DELETE FROM flinkops_templates WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete template: {e}")))?;
        Ok(())
    }

    // ---------------------------------------------------------- clusters
}
