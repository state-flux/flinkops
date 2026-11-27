//! 作业语义操作：状态机字段、restartNonce、清理旧 Pod。

#![allow(unused_imports)]
use super::*;

impl Cluster {
    /// 启停（Deployment / SessionJob 通用：都是 `spec.job.state`）。
    pub async fn set_job_state_kind(
        &self,
        kind: &str,
        ns: &str,
        name: &str,
        running: bool,
    ) -> Result<()> {
        let state = if running { "running" } else { "suspended" };
        self.patch_merge(
            kind,
            ns,
            name,
            serde_json::json!({ "spec": { "job": { "state": state } } }),
        )
        .await
        .map(|_| ())
    }

    /// 列出某集群全部作业（Deployment + SessionJob），并归一化成列表行。
    pub async fn list_jobs(&self, ns: Option<&str>) -> Result<Vec<JobSummary>> {
        let mut out = Vec::new();
        for (kind, k) in [
            (crd::KIND_DEPLOYMENT, crd::KIND_DEPLOYMENT),
            (crd::KIND_SESSION_JOB, crd::KIND_SESSION_JOB),
        ] {
            // 单个 kind 失败不该拖垮整个集群的同步：
            // 常见情况是 RBAC 只授了 FlinkDeployment，或老 operator 没有 SessionJob CRD。
            match self.list_raw(kind, ns).await {
                Ok(items) => {
                    for obj in items {
                        out.push(crd::summary(&self.id, k, &obj));
                    }
                }
                Err(Error::NotFound(_)) | Err(Error::Forbidden(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        out.sort_by(|a, b| {
            (a.namespace.clone(), a.name.clone()).cmp(&(b.namespace.clone(), b.name.clone()))
        });
        Ok(out)
    }

    /// session 模式的 FlinkDeployment（用于「会话集群」页）。
    pub async fn list_sessions(&self, ns: Option<&str>) -> Result<Vec<Value>> {
        let items = self.list_raw(crd::KIND_DEPLOYMENT, ns).await?;
        Ok(items
            .into_iter()
            // session cluster = 没有 job 段（mode 仍是 native）
            .filter(|o| o.get("spec").and_then(|s| s.get("job")).is_none())
            .collect())
    }

    // ---------------------------------------------------------- write

    /// 启动前清理残留资源。
    ///
    /// suspend 之后 operator 会删 JM Deployment，但实测会漏下 `<name>-rest` Service；
    /// 再启动时 Flink 的 `KubernetesClusterDescriptor` 认为"集群已存在"，抛
    /// `ClusterDeploymentException: The Flink cluster <name> already exists`，
    /// 且 operator 会一直重试同一个错误（不会自愈）。这里在起作业前把
    /// "JM 已不在、但 -rest Service 还在"的残留清掉。
    ///
    /// 返回被清理的资源名；没有残留返回空 Vec。清理失败不阻断启动（交给 operator 报错）。
    pub async fn clear_stale_before_start(&self, ns: &str, name: &str) -> Result<Vec<String>> {
        use k8s_openapi::api::apps::v1::Deployment;
        use k8s_openapi::api::core::v1::Service;

        let dep_api: Api<Deployment> = Api::namespaced(self.client.clone(), ns);
        let svc_api: Api<Service> = Api::namespaced(self.client.clone(), ns);
        let mut cleared = Vec::new();

        // JM 还在（哪怕 replicas=0 正在滚动）就不要动，属于正常升级轨迹
        match dep_api.get_opt(name).await {
            Ok(Some(_)) => return Ok(cleared),
            Ok(None) => {}
            Err(e) => return Err(map_err("deployment", e)),
        }

        let rest = format!("{name}-rest");
        if svc_api.get_opt(&rest).await.ok().flatten().is_some()
            && svc_api
                .delete(&rest, &kube::api::DeleteParams::default())
                .await
                .is_ok()
        {
            cleared.push(rest);
        }
        Ok(cleared)
    }

    /// 启停作业：改 `spec.job.state`（唯一官方入口）。
    pub async fn set_job_state(&self, ns: &str, name: &str, running: bool) -> Result<()> {
        let state = if running { "running" } else { "suspended" };
        self.patch_merge(
            crd::KIND_DEPLOYMENT,
            ns,
            name,
            serde_json::json!({ "spec": { "job": { "state": state } } }),
        )
        .await
        .map(|_| ())
    }

    /// 重启 = bump `spec.restartNonce`（重建 JM，不动状态）。
    ///
    /// 用「读 → 改 → 带 resourceVersion 整体 replace」做 CAS：
    /// merge patch 没有前置条件，两个并发请求会都读到 N 都写 N+1，
    /// 结果 5 次点击只重启 1 次，而每个请求都返回成功。
    pub async fn bump_restart_nonce(&self, ns: &str, name: &str) -> Result<i64> {
        self.bump_restart_nonce_kind(crd::KIND_DEPLOYMENT, ns, name)
            .await
    }

    /// 同上，但按 kind（SessionJob 也有 `spec.restartNonce`）。
    ///
    /// **冲突不重试**：重启是"用户点了一次就该生效一次"的动作。
    /// 早先这里对 409 做 `re-read + 重试`，导致 3 个并发/连点请求各自把
    /// nonce 加一次（实测 6→9），operator 连做 3 轮滚动升级，最后一批
    /// 引用了已被 retention 清掉的 checkpoint，直接把作业打成 FAILED。
    /// 现在冲突即返回 409，由调用方决定要不要重试。
    pub async fn bump_restart_nonce_kind(&self, kind: &str, ns: &str, name: &str) -> Result<i64> {
        self.ensure_writable()?;
        let mut obj = self.get_raw(kind, ns, name).await?;
        let current = crd::i(&obj, &["spec", "restartNonce"]).unwrap_or(0);
        let next = current + 1;
        obj["spec"]["restartNonce"] = serde_json::json!(next);
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::Internal(format!("serialize {name}: {e}")))?;
        match self
            .api(kind, Some(ns))
            .replace(name, &PostParams::default(), &dyn_obj)
            .await
        {
            Ok(_) => Ok(next),
            Err(kube::Error::Api(ae)) if ae.code == 409 => Err(Error::Conflict(format!(
                "{ns}/{name} 正被并发修改（resourceVersion 冲突），本次重启未生效，请刷新后再试"
            ))),
            Err(e) => Err(map_err(kind, e)),
        }
    }

    // ---------------------------------------------------------- pods / logs
}
