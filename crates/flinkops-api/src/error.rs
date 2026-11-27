//! HTTP 错误映射。
//!
//! 所有 handler 返回 `ApiResult<T>`；领域层的 `flinkops_types::Error` 在这里
//! 统一变成 HTTP 状态码 + 固定 JSON 结构：
//! `{"error":{"code":"...","message":"..."}}`（前端 `ApiError` 按此解析）。
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use flinkops_types::Error;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError(pub Error);

pub type ApiResult<T> = Result<T, ApiError>;

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = json!({
            "error": { "code": self.0.code(), "message": self.0.message() }
        });
        (status, Json(body)).into_response()
    }
}

/// 便捷构造：把任意 Display 错误包成 500。
pub fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(Error::Internal(e.to_string()))
}

// 让 ApiError 能直接用于 tracing（warn!(error = %e)）与 anyhow 上下文
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ApiError {}
