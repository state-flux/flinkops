//! CRD 通用读写：get / list / patch / replace / create / dry-run / delete。

#![allow(unused_imports)]
use super::*;

impl Cluster {
    /// 列出一类对象（自动翻页）。
    ///
    /// 单次 list 不带 limit 会把整类对象一次拉回来：51 个作业没事，
    /// 1000+ 作业时 apiserver/etcd 与平台内存都会吃紧。这里按 500 一页取完。
    pub async fn list_raw(&self, kind: &str, ns: Option<&str>) -> Result<Vec<Value>> {
        let api = self.api(kind, ns);
        let mut out: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params = ListParams::default().limit(500);
            if let Some(t) = &token {
                params = params.continue_token(t);
            }
            let page = kube_io(kind, api.list(&params)).await?;
            for o in page.items {
                out.push(
                    serde_json::to_value(&o)
                        .map_err(|e| Error::Internal(format!("serialize {kind}: {e}")))?,
                );
            }
            match page.metadata.continue_ {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    pub async fn get_raw(&self, kind: &str, ns: &str, name: &str) -> Result<Value> {
        let obj = kube_io(kind, self.api(kind, Some(ns)).get(name)).await?;
        serde_json::to_value(&obj).map_err(|e| Error::Internal(format!("serialize {kind}: {e}")))
    }

    /// 按名字取对象，不预设 kind：先找 FlinkDeployment，再找 FlinkSessionJob。
    ///
    /// 列表里两种 kind 都有，早期所有按名字的端点硬编码 Deployment，
    /// 导致 SessionJob 行"可见不可管"（点进去 404）。
    pub async fn get_any(&self, ns: &str, name: &str) -> Result<(String, Value)> {
        match self.get_raw(crd::KIND_DEPLOYMENT, ns, name).await {
            Ok(o) => Ok((crd::KIND_DEPLOYMENT.to_string(), o)),
            Err(Error::NotFound(_)) => self
                .get_raw(crd::KIND_SESSION_JOB, ns, name)
                .await
                .map(|o| (crd::KIND_SESSION_JOB.to_string(), o)),
            Err(e) => Err(e),
        }
    }

    pub async fn patch_merge(
        &self,
        kind: &str,
        ns: &str,
        name: &str,
        patch: Value,
    ) -> Result<Value> {
        self.ensure_writable()?;
        let obj = kube_io(
            kind,
            self.api(kind, Some(ns))
                .patch(name, &PatchParams::default(), &Patch::Merge(&patch)),
        )
        .await?;
        serde_json::to_value(&obj).map_err(|e| Error::Internal(format!("serialize {kind}: {e}")))
    }

    /// 带 `metadata.resourceVersion` 前置条件的 merge patch。
    ///
    /// merge patch 本身不带 CAS；把 RV 写进 patch 体后，apiserver 在 RV 不匹配时
    /// 返回 409（`kubectl patch --resource-version` 走的就是这条路），
    /// 这样"两个窗口同时改同一个 CR"不会静默覆盖，而是后到者失败。
    pub async fn patch_merge_cas(
        &self,
        kind: &str,
        ns: &str,
        name: &str,
        rv: &str,
        patch: Value,
    ) -> Result<Value> {
        let mut p = patch;
        if !rv.is_empty() {
            if !p.is_object() {
                p = serde_json::json!({});
            }
            if p.get("metadata").is_none() {
                p["metadata"] = serde_json::json!({});
            }
            p["metadata"]["resourceVersion"] = serde_json::json!(rv);
        }
        self.patch_merge(kind, ns, name, p).await
    }

    /// 创建对象（**不是** server-side apply）：重名由 apiserver 返回 409。
    pub async fn create_object(&self, kind: &str, ns: &str, obj: Value) -> Result<Value> {
        self.ensure_writable()?;
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::BadRequest(format!("invalid {kind} object: {e}")))?;
        let out = kube_io(
            kind,
            self.api(kind, Some(ns))
                .create(&PostParams::default(), &dyn_obj),
        )
        .await?;
        serde_json::to_value(&out).map_err(|e| Error::Internal(format!("serialize {kind}: {e}")))
    }

    /// 服务端 dry-run 创建：apiserver 会跑 CRD schema + webhook，但不落库。
    /// 平台的"预检"用它，才能拿到和真实提交一致的裁决。
    pub async fn create_object_dryrun(&self, kind: &str, ns: &str, obj: Value) -> Result<Value> {
        self.ensure_writable()?;
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::BadRequest(format!("invalid {kind} object: {e}")))?;
        let pp = PostParams {
            dry_run: true,
            ..Default::default()
        };
        let out = kube_io(kind, self.api(kind, Some(ns)).create(&pp, &dyn_obj)).await?;
        Ok(serde_json::to_value(&out).unwrap_or(Value::Null))
    }

    /// 整体 replace（带 `metadata.resourceVersion` 即为 CAS）。
    pub async fn replace_object(
        &self,
        kind: &str,
        ns: &str,
        name: &str,
        obj: Value,
    ) -> Result<Value> {
        self.ensure_writable()?;
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::BadRequest(format!("invalid {kind} object: {e}")))?;
        let out = self
            .api(kind, Some(ns))
            .replace(name, &PostParams::default(), &dyn_obj)
            .await
            .map_err(|e| map_err(kind, e))?;
        serde_json::to_value(&out).map_err(|e| Error::Internal(format!("serialize {kind}: {e}")))
    }

    /// 删除资源（作业 / 快照）。
    pub async fn delete_object(&self, kind: &str, ns: &str, name: &str) -> Result<()> {
        self.ensure_writable()?;
        self.api(kind, Some(ns))
            .delete(name, &kube::api::DeleteParams::default())
            .await
            .map_err(|e| map_err(kind, e))?;
        Ok(())
    }

    /// 任意 CRD 的 server-side apply（PodMonitor 等第三方 CR 用）。
    pub async fn apply_custom(
        &self,
        group: &str,
        version: &str,
        kind: &str,
        ns: &str,
        obj: Value,
    ) -> Result<String> {
        self.ensure_writable()?;
        let name = obj
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::BadRequest("metadata.name 不能为空".into()))?
            .to_string();
        let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), ns, &ar);
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::BadRequest(format!("invalid {kind}: {e}")))?;
        api.patch(
            &name,
            &PatchParams::apply("flinkops").force(),
            &Patch::Apply(&dyn_obj),
        )
        .await
        .map_err(|e| map_err(kind, e))?;
        Ok(name)
    }
}
