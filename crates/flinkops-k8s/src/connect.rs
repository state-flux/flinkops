//! 接入与基础：默认/kubeconfig 两种 client 构造、Api 包装、只读守卫。

#![allow(unused_imports)]
use super::*;

impl Cluster {
    pub async fn try_default(id: impl Into<String>, readonly: bool) -> Result<Self> {
        let client = Client::try_default()
            .await
            .map_err(|e| Error::Internal(format!("kube client (in-cluster/default): {e}")))?;
        Ok(Self {
            id: id.into(),
            client,
            readonly,
            in_cluster: true,
            api_host: None,
        })
    }

    /// 从 kubeconfig 文件接入（跨集群场景）。
    pub async fn from_kubeconfig(
        id: impl Into<String>,
        path: &str,
        readonly: bool,
    ) -> Result<Self> {
        let kubeconfig = kube::config::Kubeconfig::read_from(path)
            .map_err(|e| Error::Internal(format!("read kubeconfig {path}: {e}")))?;
        let opts = kube::config::KubeConfigOptions::default();
        let config = kube::Config::from_custom_kubeconfig(kubeconfig, &opts)
            .await
            .map_err(|e| Error::Internal(format!("parse kubeconfig {path}: {e}")))?;
        // kubeconfig 的 server 地址（NodePort 用它拼可达 IP）
        let api_host = config.cluster_url.host().map(str::to_string);
        let client =
            Client::try_from(config).map_err(|e| Error::Internal(format!("build client: {e}")))?;
        Ok(Self {
            id: id.into(),
            client,
            readonly,
            in_cluster: false,
            api_host,
        })
    }

    /// 从 kubeconfig **文本**接入（"粘贴 kubeconfig" 场景）。
    ///
    /// 与 `from_kubeconfig` 的唯一区别是数据来源：这里直接解析 YAML，
    /// 不需要先落盘再读回来。`origin` 只用于错误信息，便于定位是哪份配置。
    pub async fn from_kubeconfig_text(
        id: impl Into<String>,
        text: &str,
        origin: &str,
        readonly: bool,
    ) -> Result<Self> {
        let kubeconfig: kube::config::Kubeconfig = serde_yaml::from_str(text)
            .map_err(|e| Error::BadRequest(format!("kubeconfig 解析失败（{origin}）：{e}")))?;
        let opts = kube::config::KubeConfigOptions::default();
        let config = kube::Config::from_custom_kubeconfig(kubeconfig, &opts)
            .await
            .map_err(|e| Error::BadRequest(format!("kubeconfig 无效（{origin}）：{e}")))?;
        let api_host = config.cluster_url.host().map(str::to_string);
        let client =
            Client::try_from(config).map_err(|e| Error::Internal(format!("build client: {e}")))?;
        Ok(Self {
            id: id.into(),
            client,
            readonly,
            in_cluster: false,
            api_host,
        })
    }

    pub(crate) fn ar(kind: &str) -> ApiResource {
        ApiResource::from_gvk(&GroupVersionKind::gvk(crd::GROUP, crd::VERSION, kind))
    }

    pub(crate) fn api(&self, kind: &str, ns: Option<&str>) -> Api<DynamicObject> {
        let ar = Self::ar(kind);
        match ns {
            Some(ns) => Api::namespaced_with(self.client.clone(), ns, &ar),
            None => Api::all_with(self.client.clone(), &ar),
        }
    }

    // ---------------------------------------------------------- read

    pub(crate) fn ensure_writable(&self) -> Result<()> {
        if self.readonly {
            return Err(Error::Forbidden(format!(
                "cluster {} 以只读模式接入，禁止写操作",
                self.id
            )));
        }
        Ok(())
    }
}
