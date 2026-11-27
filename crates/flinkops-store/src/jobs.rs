//! 作业索引：全量对账、按簇替换、分页查询。

#![allow(unused_imports)]
use super::*;

impl Store {
    pub async fn upsert_job(&self, j: &JobSummary) -> Result<()> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Internal(format!("acquire conn: {e}")))?;
        upsert_job_on(&mut conn, self.dialect, j).await
    }

    /// 事务内全量替换某个（集群 | 集群+命名空间）的索引。
    ///
    /// 原来 `DELETE` 与逐条 `upsert` 是两条独立语句：并发刷新会互相删行，
    /// 进程在中间退出会让该集群索引归零，读者还可能看到"删完了、还没写回"的空窗。
    /// 放进一个事务后，读者要么看到旧集合、要么看到新集合。
    ///
    /// 返回 (删除行数, 写入行数)。
    pub async fn replace_cluster_jobs(
        &self,
        cluster: &str,
        ns: Option<&str>,
        jobs: &[JobSummary],
    ) -> Result<(u64, usize)> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::Internal(format!("begin tx: {e}")))?;
        let deleted = match ns {
            Some(n) => {
                sqlx::query(&self.sql("DELETE FROM flinkops_jobs WHERE cluster = ? AND ns = ?"))
                    .bind(cluster)
                    .bind(n)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| Error::Internal(format!("prune ns index: {e}")))?
                    .rows_affected()
            }
            None => sqlx::query(&self.sql("DELETE FROM flinkops_jobs WHERE cluster = ?"))
                .bind(cluster)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::Internal(format!("prune index: {e}")))?
                .rows_affected(),
        };
        for j in jobs {
            upsert_job_on(&mut tx, self.dialect, j).await?;
        }
        tx.commit()
            .await
            .map_err(|e| Error::Internal(format!("commit sync: {e}")))?;
        Ok((deleted, jobs.len()))
    }

    // `..Default::default()` 是**故意**保留的：JobSummary 以后加字段时，
    // 这里会自动补默认值而不是编译失败（列表页不该因为一个可选字段挂掉）。
    #[allow(clippy::needless_update)]
    /// 索引里出现过的集群（用于清理"集群已移除但索引还在"的孤儿）。
    pub async fn list_job_clusters(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(&self.sql("SELECT DISTINCT cluster FROM flinkops_jobs"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("list job clusters: {e}")))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>("cluster").ok())
            .collect())
    }

    /// 清掉某个集群的全部索引（删集群 / 全量同步前用）。
    pub async fn delete_jobs_by_cluster(&self, cluster: &str) -> Result<u64> {
        let res = sqlx::query(&self.sql("DELETE FROM flinkops_jobs WHERE cluster = ?"))
            .bind(cluster)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("delete jobs by cluster: {e}")))?;
        Ok(res.rows_affected())
    }

    /// 删单条作业索引（删除作业后立刻调用，避免列表里留"幽灵行"）。
    pub async fn delete_job(&self, cluster: &str, ns: &str, name: &str) -> Result<u64> {
        let res = sqlx::query(
            &self.sql("DELETE FROM flinkops_jobs WHERE cluster = ? AND ns = ? AND name = ?"),
        )
        .bind(cluster)
        .bind(ns)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("delete job: {e}")))?;
        Ok(res.rows_affected())
    }

    pub async fn list_jobs(&self, cluster: Option<&str>) -> Result<Vec<JobSummary>> {
        Ok(self.list_jobs_page(cluster, None, None, 0).await?.0)
    }

    /// 带过滤/分页的作业索引查询。
    ///
    /// 返回 (本页行, 过滤后的总数)。`ns`/`limit` 下推到 SQL：集群上千作业时
    /// 原来会把整表读进内存再过滤，列表页还没分页就先 OOM 了。
    pub async fn list_jobs_page(
        &self,
        cluster: Option<&str>,
        ns: Option<&str>,
        limit: Option<i64>,
        offset: i64,
    ) -> Result<(Vec<JobSummary>, i64)> {
        let lim = limit.unwrap_or(5000).clamp(1, 20_000);
        let rows = match (cluster, ns) {
            (Some(c), Some(n)) => {
                sqlx::query(&self.sql("SELECT * FROM flinkops_jobs WHERE cluster = ? AND ns = ? ORDER BY state, name LIMIT ? OFFSET ?"),
                )
                .bind(c)
                .bind(n)
                .bind(lim)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (Some(c), None) => {
                sqlx::query(&self.sql("SELECT * FROM flinkops_jobs WHERE cluster = ? ORDER BY state, name LIMIT ? OFFSET ?"),
                )
                .bind(c)
                .bind(lim)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (None, Some(n)) => {
                sqlx::query(&self.sql("SELECT * FROM flinkops_jobs WHERE ns = ? ORDER BY cluster, name LIMIT ? OFFSET ?"),
                )
                .bind(n)
                .bind(lim)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (None, None) => {
                sqlx::query(&self.sql("SELECT * FROM flinkops_jobs ORDER BY cluster, ns, name LIMIT ? OFFSET ?"),
                )
                .bind(lim)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| Error::Internal(format!("list jobs: {e}")))?;

        // 总数（前端显示"共 N 行 / 分页"用）
        let total: i64 = match (cluster, ns) {
            (Some(c), Some(n)) => {
                sqlx::query_scalar::<_, i64>(
                    &self.sql("SELECT COUNT(*) FROM flinkops_jobs WHERE cluster = ? AND ns = ?"),
                )
                .bind(c)
                .bind(n)
                .fetch_one(&self.pool)
                .await
            }
            (Some(c), None) => {
                sqlx::query_scalar::<_, i64>(
                    &self.sql("SELECT COUNT(*) FROM flinkops_jobs WHERE cluster = ?"),
                )
                .bind(c)
                .fetch_one(&self.pool)
                .await
            }
            (None, Some(n)) => {
                sqlx::query_scalar::<_, i64>(
                    &self.sql("SELECT COUNT(*) FROM flinkops_jobs WHERE ns = ?"),
                )
                .bind(n)
                .fetch_one(&self.pool)
                .await
            }
            (None, None) => {
                sqlx::query_scalar::<_, i64>(&self.sql("SELECT COUNT(*) FROM flinkops_jobs"))
                    .fetch_one(&self.pool)
                    .await
            }
        }
        .map_err(|e| Error::Internal(format!("count jobs: {e}")))?;

        let out = rows
            .into_iter()
            .map(|r| JobSummary {
                cluster: r.try_get("cluster").unwrap_or_default(),
                namespace: r.try_get("ns").unwrap_or_default(),
                name: r.try_get("name").unwrap_or_default(),
                kind: r.try_get("kind").unwrap_or_default(),
                state: r.try_get("state").unwrap_or_default(),
                job_id: r.try_get("job_id").ok(),
                parallelism: r.try_get("parallelism").ok(),
                // 索引表不存 job 图并行度（要连 REST 才有），列表页不展示
                parallelism_actual: None,
                task_manager_replicas: r.try_get("tm_replicas").ok(),
                slots_per_tm: r.try_get("slots_per_tm").ok(),
                restart_nonce: r.try_get("restart_nonce").ok(),
                upgrade_mode: r.try_get("upgrade_mode").ok(),
                image: r.try_get("image").ok(),
                job_manager_cpu: r.try_get("jm_cpu").ok(),
                job_manager_memory: r.try_get("jm_mem").ok(),
                task_manager_cpu: r.try_get("tm_cpu").ok(),
                task_manager_memory: r.try_get("tm_mem").ok(),
                flink_version: r.try_get("flink_version").ok(),
                rest_exposed: r.try_get("rest_exposed").ok(),
                job_manager_status: r.try_get("jm_status").ok(),
                reconciliation_state: r.try_get("recon_state").ok(),
                error: crate::text_col_opt(&r, "err_text"),
                generation: r.try_get("generation").ok(),
                observed_generation: r.try_get("observed_generation").ok(),
                conditions: serde_json::from_str(&crate::text_col(&r, "conditions"))
                    .unwrap_or_default(),
                lifecycle_state: r.try_get("lifecycle_state").ok(),
                started_at: r.try_get("started_at").ok(),
                updated_at: r.try_get("updated_at").ok(),
            })
            .collect();
        Ok((out, total))
    }

    // ---------------------------------------------------------- audit
}
