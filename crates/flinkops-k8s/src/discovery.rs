//! 环境探测：operator、Prometheus、CRD schema、命名空间与 ServiceAccount。

#![allow(unused_imports)]
use super::*;

/// 探活结果：接入前"测试连通性"用，不落库、不改集群。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    /// apiserver 版本，如 `v1.32.6`
    pub server_version: String,
    /// 能看到的 FlinkDeployment / FlinkSessionJob 总数
    pub job_count: usize,
    /// 该集群支持的 CRD kind（拿不到就空）
    pub kinds: Vec<String>,
}

impl Cluster {
    /// 轻量探活：版本 + 作业数 + 支持的 kind。
    ///
    /// 只做读操作，不需要 `ensure_writable`；失败信息原样带回给调用方，
    /// 便于在"接入集群"弹窗里直接显示原因。
    pub async fn probe(&self) -> Result<ProbeReport> {
        let version = self
            .client
            .apiserver_version()
            .await
            .map_err(|e| map_err("apiserver", e))?;
        let server_version = if version.git_version.is_empty() {
            format!("{}.{}", version.major, version.minor)
        } else {
            version.git_version.clone()
        };
        let kinds = self.supported_kinds().await.unwrap_or_default();
        let job_count = self.list_jobs(None).await.map(|v| v.len()).unwrap_or(0);
        Ok(ProbeReport {
            server_version,
            job_count,
            kinds,
        })
    }
}

impl Cluster {
    /// 列出命名空间里的 ServiceAccount（创建 Pod 前必须存在，否则 Pod 起不来）。
    pub async fn list_service_accounts(&self, ns: &str) -> Result<Vec<String>> {
        use k8s_openapi::api::core::v1::ServiceAccount;
        let api: Api<ServiceAccount> = Api::namespaced(self.client.clone(), ns);
        let list = api
            .list(&kube::api::ListParams::default())
            .await
            .map_err(|e| map_err("serviceaccount", e))?;
        Ok(list
            .items
            .into_iter()
            .filter_map(|sa| sa.metadata.name)
            .collect())
    }

    /// 读某个 CRD 的 spec 字段清单（诊断用：字段名猜错时一眼看真相）。
    pub async fn crd_spec_schema(
        &self,
        crd_name: &str,
    ) -> Result<(Vec<String>, Vec<String>, Value)> {
        use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
        let api: Api<CustomResourceDefinition> = Api::all(self.client.clone());
        let crd = api.get(crd_name).await.map_err(|e| map_err("crd", e))?;

        let mut spec_keys = Vec::new();
        let mut required = Vec::new();
        let mut json_schema_spec: Option<Value> = None;
        if let Some(versions) = crd.spec.versions.iter().find(|v| v.served && v.storage) {
            if let Some(schema) = &versions.schema {
                let json = serde_json::to_value(&schema.open_api_v3_schema).unwrap_or(Value::Null);
                json_schema_spec = json.pointer("/properties/spec").cloned();
                if let Some(props) = json
                    .pointer("/properties/spec/properties")
                    .and_then(|v| v.as_object())
                {
                    spec_keys = props.keys().cloned().collect();
                }
                if let Some(req) = json
                    .pointer("/properties/spec/required")
                    .and_then(|v| v.as_array())
                {
                    required = req
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                }
            }
        }
        let detail = json_schema_spec.unwrap_or(Value::Null);
        Ok((spec_keys, required, detail))
    }

    /// 命名空间是否存在（创建资源前必查，否则 apply 会报 `namespaces "x" not found`）。
    pub async fn namespace_exists(&self, ns: &str) -> Result<bool> {
        use k8s_openapi::api::core::v1::Namespace;
        let api: Api<Namespace> = Api::all(self.client.clone());
        match api.get(ns).await {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(false),
            Err(e) => Err(map_err("namespace", e)),
        }
    }

    /// 逐个尝试 list，判断集群支持哪些 Flink CRD（版本差异大，探测比猜可靠）。
    pub async fn supported_kinds(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for kind in [
            crd::KIND_DEPLOYMENT,
            crd::KIND_SESSION_JOB,
            crd::KIND_SNAPSHOT,
            crd::KIND_BLUE_GREEN,
        ] {
            if self.list_raw(kind, None).await.is_ok() {
                out.push(kind.to_string());
            }
        }
        Ok(out)
    }

    /// 找 operator 自己的 Deployment，拿镜像 tag（判断实际升级到哪个版本）。
    pub async fn find_operator(&self) -> Result<Vec<OperatorInfo>> {
        use k8s_openapi::api::apps::v1::Deployment;

        let api: Api<Deployment> = Api::all(self.client.clone());
        let list = api
            .list(&ListParams::default())
            .await
            .map_err(|e| map_err("deployments", e))?;

        let mut out = Vec::new();
        for d in list.items {
            let name = d.metadata.name.clone().unwrap_or_default();
            if !name
                .to_ascii_lowercase()
                .contains("flink-kubernetes-operator")
            {
                continue;
            }
            let images: Vec<String> = d
                .spec
                .as_ref()
                .map(|s| {
                    s.template
                        .spec
                        .as_ref()
                        .map(|ps| {
                            ps.containers
                                .iter()
                                .filter_map(|c| c.image.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            let ready = d
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            out.push(OperatorInfo {
                namespace: d.metadata.namespace.clone().unwrap_or_default(),
                name,
                images,
                ready_replicas: ready as i64,
            });
        }
        Ok(out)
    }

    /// 找集群里的 Prometheus：列出 Service，按名字匹配（prometheus 相关）。
    ///
    /// 不依赖标签约定（不同 chart 标签不一样），只看名字；小集群几百个 Service
    /// 一次 list 足够快，结果由调用方缓存。
    pub async fn find_prometheus(&self) -> Result<Vec<PromCandidate>> {
        use k8s_openapi::api::core::v1::Service;

        let api: Api<Service> = Api::all(self.client.clone());
        let list = api
            .list(&ListParams::default())
            .await
            .map_err(|e| map_err("services", e))?;

        let mut out = Vec::new();
        for svc in list.items {
            let name = svc.metadata.name.clone().unwrap_or_default();
            let lower = name.to_ascii_lowercase();
            // 只要真正的 server，不要 alertmanager/operator/exporter 之类
            if !lower.contains("prometheus")
                || lower.contains("alertmanager")
                || lower.contains("operator")
            {
                continue;
            }
            let spec = svc.spec.clone().unwrap_or_default();
            let ports = spec.ports.clone().unwrap_or_default();
            let Some(p) = ports
                .iter()
                .find(|p| p.port == 9090)
                .or_else(|| ports.first())
            else {
                continue;
            };
            out.push(PromCandidate {
                namespace: svc.metadata.namespace.clone().unwrap_or_default(),
                name,
                port: p.port,
                node_port: p.node_port,
                service_type: spec.type_.clone().unwrap_or_else(|| "ClusterIP".into()),
            });
        }
        Ok(out)
    }

    /// 把候选 Service 变成可访问的 base URL（平台在集群外时优先 NodePort）。
    pub fn prom_base_url(&self, c: &PromCandidate) -> Option<String> {
        if self.in_cluster {
            return Some(format!(
                "http://{}.{}.svc.cluster.local:{}",
                c.name, c.namespace, c.port
            ));
        }
        let host = self.api_host.clone()?;
        if host == "127.0.0.1" || host == "localhost" || host == "::1" {
            return None;
        }
        c.node_port.map(|np| format!("http://{host}:{np}"))
    }
}
