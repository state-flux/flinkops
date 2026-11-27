//! 存活/就绪/版本。**不需要登录**（探针与静态页要用）。
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;

use crate::state::AppState;

pub async fn healthz() -> &'static str {
    "ok"
}

/// 就绪 = DB 可读 + 至少一个集群可用。
pub async fn readyz(State(st): State<Arc<AppState>>) -> Response {
    let db_ok = st.store.list_profiles().await.is_ok();
    let cluster_ok = st.has_clusters().await;
    if db_ok && cluster_ok {
        (StatusCode::OK, "ready").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "db": db_ok, "clusters": cluster_ok })),
        )
            .into_response()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub name: &'static str,
    pub version: &'static str,
    /// flink-rest | prometheus —— 前端据此决定大盘是否显示历史
    pub metrics_source: &'static str,
    pub clusters: usize,
    /// 元数据后端：sqlite | postgres（由 FLINKOPS_STORE 决定）
    pub store_backend: &'static str,
}

pub async fn version(State(st): State<Arc<AppState>>) -> Json<VersionInfo> {
    Json(VersionInfo {
        name: "flinkops",
        version: env!("CARGO_PKG_VERSION"),
        metrics_source: if st.cfg.prometheus_url.is_some() {
            "prometheus"
        } else {
            "flink-rest"
        },
        clusters: st.clusters_count().await,
        store_backend: st.store.dialect().as_str(),
    })
}
