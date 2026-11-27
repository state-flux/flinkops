//! 平台 HTTP 客户端：只带 Token，不碰 kubeconfig（见 `docs/mcp-spec.md §0`）。

use anyhow::{anyhow, Result};
use reqwest::Method;
use serde_json::Value;

#[derive(Clone)]
pub struct Platform {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Platform {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        approval: Option<&str>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(&self.token)
            .header("accept", "application/json");
        if let Some(token) = approval {
            req = req.header("x-flinkops-approval", token);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| anyhow!("平台不可达：{e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: Value =
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()));
        if status.is_success() {
            return Ok(parsed);
        }
        // 把平台的结构化错误原样交给模型：approval_required / denied / unavailable 都要可读
        let code = parsed
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str())
            .unwrap_or("http_error");
        let msg = parsed
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(text.as_str());
        Err(anyhow!("{code}（HTTP {status}）：{msg}"))
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.send(Method::GET, path, None, None).await
    }

    pub async fn post(&self, path: &str, body: Value, approval: Option<&str>) -> Result<Value> {
        self.send(Method::POST, path, Some(body), approval).await
    }

    pub async fn delete(&self, path: &str, approval: Option<&str>) -> Result<Value> {
        self.send(Method::DELETE, path, None, approval).await
    }
}
