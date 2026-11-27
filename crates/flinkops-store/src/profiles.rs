//! 环境基线（Profile）存取。

#![allow(unused_imports)]
use super::*;

impl Store {
    pub async fn save_profile(&self, p: &Profile) -> Result<()> {
        let payload = serde_json::to_string(p).map_err(Error::from)?;
        let now = flinkops_types::now_rfc3339();
        let updated = sqlx::query(&self.sql(
            "UPDATE flinkops_profiles SET name=?, payload=?, builtin=?, updated_at=? WHERE id=?",
        ))
        .bind(&p.name)
        .bind(&payload)
        .bind(if p.builtin { 1i64 } else { 0i64 })
        .bind(&now)
        .bind(&p.id)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("update profile: {e}")))?;

        if updated.rows_affected() == 0 {
            sqlx::query(&self.sql("INSERT INTO flinkops_profiles (id, name, payload, builtin, updated_at) VALUES (?,?,?,?,?)"),
            )
            .bind(&p.id)
            .bind(&p.name)
            .bind(&payload)
            .bind(if p.builtin { 1i64 } else { 0i64 })
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("insert profile: {e}")))?;
        }
        Ok(())
    }

    pub async fn list_profiles(&self) -> Result<Vec<Profile>> {
        let rows = sqlx::query(
            &self.sql("SELECT payload FROM flinkops_profiles ORDER BY builtin DESC, name"),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("list profiles: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| crate::text_col(&r, "payload"))
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect())
    }

    pub async fn seed_default_profile(&self) -> Result<()> {
        if self.list_profiles().await?.is_empty() {
            self.save_profile(&Profile::default()).await?;
        }
        Ok(())
    }

    // ---------------------------------------------------------- MCP tokens / approvals
}
