//! 导出：结果集 → xlsx（走 python openpyxl，避免给 Rust 加 xlsx 依赖）。
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use flinkops_types::Error;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XlsxBody {
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
    pub sheet: Option<String>,
    pub filename: Option<String>,
}

/// POST /api/export/xlsx —— 把前端拿到结果集转成 Excel 下载
pub async fn xlsx(
    State(_st): State<Arc<AppState>>,
    u: AuthUser,
    Json(body): Json<XlsxBody>,
) -> ApiResult<Response> {
    let AuthUser { headers, .. } = u;

    let _ = _st.require_user(&headers).await?;
    if body.columns.is_empty() {
        return Err(ApiError(Error::BadRequest("columns 不能为空".into())));
    }

    let script = [
        "scripts/to_xlsx.py",
        "../scripts/to_xlsx.py",
        "../../scripts/to_xlsx.py",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|p| p.exists())
    .ok_or_else(|| ApiError(Error::Internal("找不到 scripts/to_xlsx.py".into())))?;

    let payload = serde_json::json!({
        "columns": body.columns,
        "rows": body.rows,
        "sheet": body.sheet.unwrap_or_else(|| "result".into()),
    });
    let input = serde_json::to_vec(&payload)
        .map_err(|e| ApiError(Error::Internal(format!("序列化失败：{e}"))))?;

    // 用管道把 JSON 喂给 python，读回二进制
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = tokio::process::Command::new(python)
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ApiError(Error::Internal(format!("调用 {python} 失败：{e}"))))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| ApiError(Error::Internal(format!("等待 python 失败：{e}"))))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(ApiError(Error::Internal(format!(
            "生成 Excel 失败：{}",
            err.chars().take(300).collect::<String>()
        ))));
    }
    if out.stdout.is_empty() {
        return Err(ApiError(Error::Internal("生成 Excel 返回空".into())));
    }

    let name = body.filename.unwrap_or_else(|| {
        format!(
            "flinkops-{}.xlsx",
            chrono::Utc::now().format("%Y%m%d%H%M%S")
        )
    });
    let name = if name.ends_with(".xlsx") {
        name
    } else {
        format!("{name}.xlsx")
    };

    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        out.stdout,
    )
        .into_response())
}
