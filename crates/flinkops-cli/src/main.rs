//! `flinkopsctl` —— 运维小工具：自检、列作业、导出。
//!
//! 与 Web 共用同一套领域逻辑（`flinkops-core`），所以 CLI 里做的校验
//! 和页面里看到的结论一致。
use std::path::Path;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "flinkopsctl", version, about = "FlinkOps 运维工具")]
struct Cli {
    /// 控制面地址
    #[arg(
        long,
        global = true,
        env = "FLINKOPS_ENDPOINT",
        default_value = "http://127.0.0.1:8080"
    )]
    endpoint: String,
    /// 登录用户名
    #[arg(
        long,
        global = true,
        env = "FLINKOPS_ADMIN_USER",
        default_value = "admin"
    )]
    user: String,
    /// 登录密码
    #[arg(
        long,
        global = true,
        env = "FLINKOPS_ADMIN_PASSWORD",
        default_value = "admin"
    )]
    password: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Clone)]
enum Cmd {
    /// 本地/远端自检：目录、API、默认基线校验
    Doctor,
    /// 列出作业（从控制面索引读）
    Jobs {
        #[arg(long)]
        cluster: Option<String>,
    },
    /// 导出作业列表到 JSON（便于迁移/审计留档）
    Export {
        #[arg(long, default_value = "flinkops-jobs.json")]
        out: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 纯 Rust TLS provider（rustls + RustCrypto），必须先于任何 reqwest 客户端
    flinkops_tls::install();
    let cli = Cli::parse();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    match cli.cmd.clone() {
        Cmd::Doctor => doctor(&cli, &http).await,
        Cmd::Jobs { cluster } => {
            let body = api_get(&cli, &http, "/api/jobs").await?;
            let items = body
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut shown = 0;
            for j in items {
                let c = j.get("cluster").and_then(|v| v.as_str()).unwrap_or("-");
                if let Some(want) = &cluster {
                    if want != c {
                        continue;
                    }
                }
                println!(
                    "{:<12} {:<16} {:<28} {:<12} {}",
                    c,
                    j.get("namespace").and_then(|v| v.as_str()).unwrap_or("-"),
                    j.get("name").and_then(|v| v.as_str()).unwrap_or("-"),
                    j.get("state").and_then(|v| v.as_str()).unwrap_or("-"),
                    j.get("health").and_then(|v| v.as_str()).unwrap_or("-"),
                );
                shown += 1;
            }
            println!("共 {shown} 个作业");
            Ok(())
        }
        Cmd::Export { out } => {
            let body = api_get(&cli, &http, "/api/jobs").await?;
            let text = serde_json::to_string_pretty(&body)?;
            std::fs::write(&out, text).with_context(|| format!("write {out}"))?;
            println!("已导出到 {out}");
            Ok(())
        }
    }
}

async fn login(cli: &Cli, http: &reqwest::Client) -> anyhow::Result<String> {
    let resp = http
        .post(format!("{}/api/login", cli.endpoint))
        .json(&serde_json::json!({ "username": cli.user, "password": cli.password }))
        .send()
        .await
        .with_context(|| format!("POST {}/api/login", cli.endpoint))?;
    if !resp.status().is_success() {
        anyhow::bail!("登录失败: HTTP {}", resp.status());
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or_default()
        .to_string();
    if cookie.is_empty() {
        anyhow::bail!("登录响应没有 Set-Cookie");
    }
    Ok(cookie)
}

async fn api_get(cli: &Cli, http: &reqwest::Client, path: &str) -> anyhow::Result<Value> {
    let cookie = login(cli, http).await?;
    let resp = http
        .get(format!("{}{}", cli.endpoint, path))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .with_context(|| format!("GET {}{path}", cli.endpoint))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "{path} 返回 {status}: {}",
            body.chars().take(300).collect::<String>()
        );
    }
    Ok(resp.json::<Value>().await?)
}

async fn doctor(cli: &Cli, http: &reqwest::Client) -> anyhow::Result<()> {
    println!("FlinkOps 自检");

    // 1) 默认基线能不能生成一份合法 spec（验证 core 与基线一致）
    let profile = flinkops_types::Profile::default();
    let spec = flinkops_core::spec_from_profile(
        &profile,
        &serde_json::json!({ "metadata": { "name": "smoke" } }),
    );
    let findings = flinkops_core::validate(&spec);
    let errors = findings
        .iter()
        .filter(|f| f.severity == flinkops_types::Severity::Error)
        .count();
    println!(
        "  [1] 基线 {} → 生成 spec：{} 条 finding（{} error）",
        profile.id,
        findings.len(),
        errors
    );
    if errors > 0 {
        for f in findings
            .iter()
            .filter(|f| f.severity == flinkops_types::Severity::Error)
        {
            println!("      ! {} {} {}", f.code, f.path, f.message);
        }
        anyhow::bail!("默认基线产出非法 spec");
    }

    // 2) 本地目录
    let web = std::env::var("FLINKOPS_WEB_DIR").unwrap_or_else(|_| "./web".into());
    let web_ok = Path::new(&web).join("index.html").exists();
    println!(
        "  [2] 静态目录 {web}: {}",
        if web_ok { "ok" } else { "缺少 index.html" }
    );

    // 3) 控制面可达性
    let health = http.get(format!("{}/healthz", cli.endpoint)).send().await;
    match health {
        Ok(r) if r.status().is_success() => println!("  [3] 控制面 {}: ok", cli.endpoint),
        Ok(r) => println!("  [3] 控制面 {}: HTTP {}", cli.endpoint, r.status()),
        Err(e) => println!("  [3] 控制面 {}: 不可达（{e}）", cli.endpoint),
    }

    // 4) 登录 + /api/me
    match api_get(cli, http, "/api/me").await {
        Ok(me) => println!(
            "  [4] 登录成功：user={} clusters={} prometheus={}",
            me.get("user").and_then(|v| v.as_str()).unwrap_or("-"),
            me.get("clusters")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            me.get("prometheus")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        Err(e) => println!("  [4] 登录/会话检查失败：{e}"),
    }

    if !web_ok {
        anyhow::bail!("静态目录检查失败");
    }
    println!("自检完成");
    Ok(())
}
