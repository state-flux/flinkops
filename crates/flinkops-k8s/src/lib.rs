//! Kubernetes 访问层：用**动态客户端**读写 flink.apache.org/v1beta1。
//!
//! 为什么不用 `CustomResource` derive：平台上要同时对接 1.5 / 1.9 / 2.x 的
//! operator，CRD 字段一直在长（`ingress`、`autoscalerResetNonce`、blue-green…）。
//! 动态读写 + `flinkops-types::crd` 的提取函数，比跟版本绑死的强类型更耐用。
mod apply;
mod connect;
mod discovery;
mod jobs;
mod objects;
mod runtime;

use flinkops_types::{crd, Error, EventView, JobSummary, Result};
use futures::StreamExt;
use kube::api::{
    Api, ApiResource, DynamicObject, ListParams, LogParams, Patch, PatchParams, PostParams,
};
use kube::core::GroupVersionKind;
use kube::Client;
use serde_json::Value;

/// Flink JobManager REST 的可达地址。
#[derive(Debug, Clone)]
pub enum RestEndpoint {
    /// 集群内 DNS（平台自己跑在 Pod 里时最快，且不占节点端口）
    Dns(String),
    /// 节点 IP:NodePort（平台在集群外，作业是 NodePort/LoadBalancer）
    NodePort { host: String, port: i32 },
}

impl RestEndpoint {
    pub fn base(&self) -> String {
        match self {
            RestEndpoint::Dns(u) => u.clone(),
            RestEndpoint::NodePort { host, port } => format!("http://{host}:{port}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            RestEndpoint::Dns(_) => "dns",
            RestEndpoint::NodePort { .. } => "nodeport",
        }
    }
}

/// 集群里找到的 Prometheus 候选（Service 级）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromCandidate {
    pub namespace: String,
    pub name: String,
    pub port: i32,
    pub node_port: Option<i32>,
    pub service_type: String,
}

/// operator 的部署信息（用来确认到底升级到哪个版本）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorInfo {
    pub namespace: String,
    pub name: String,
    pub images: Vec<String>,
    pub ready_replicas: i64,
}

/// apiserver 单次请求超时（kube-rs 的 Client 不提供全局 timeout）。
const KUBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 给单个 kube 调用套超时：超时统一映射成 502 upstream，
/// 避免 apiserver 半连接时把平台请求无限挂住。
async fn kube_io<T, F>(what: &str, fut: F) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, kube::Error>>,
{
    match tokio::time::timeout(KUBE_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(map_err(what, e)),
        Err(_) => Err(Error::Upstream {
            target: "kube-apiserver".into(),
            message: format!(
                "{what} 超过 {}s 未响应（apiserver 不可达或过载）",
                KUBE_TIMEOUT.as_secs()
            ),
        }),
    }
}

pub struct Cluster {
    pub id: String,
    pub client: Client,
    pub readonly: bool,
    /// 平台自身是否在集群内：决定 REST 走 DNS 还是 NodePort
    pub in_cluster: bool,
    /// kubeconfig 里 api-server 的 host（用于推算 NodePort 可达地址）
    pub api_host: Option<String>,
}

impl Cluster {}

// ---------------------------------------------------------------- watch 事件

/// watch 回调的两种语义：对象出现/变化、对象被删。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOp {
    Upsert,
    Delete,
}

fn fmt_time(t: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time) -> String {
    t.0.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn map_err(kind: &str, e: kube::Error) -> Error {
    match &e {
        kube::Error::Api(ae) if ae.code == 404 => {
            Error::NotFound(format!("{kind} not found ({})", ae.message))
        }
        kube::Error::Api(ae) if ae.code == 403 => Error::Forbidden(ae.message.clone()),
        kube::Error::Api(ae) if ae.code == 409 => Error::Conflict(ae.message.clone()),
        // 422 = 校验失败（名字不合法、字段非法）：是调用方的问题，不是上游故障
        kube::Error::Api(ae) if ae.code == 422 => Error::BadRequest(ae.message.clone()),
        kube::Error::Api(ae) => Error::Upstream {
            target: format!("kube-apiserver/{kind}"),
            message: format!("{} {}", ae.code, ae.message),
        },
        other => Error::Upstream {
            target: format!("kube-apiserver/{kind}"),
            message: other.to_string(),
        },
    }
}
