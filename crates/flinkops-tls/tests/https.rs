//! 端到端验证：纯 Rust provider 能完成真实 TLS 握手并校验证书。
//!
//! 默认 `#[ignore]`（CI 不依赖外网），本地验证：
//! `cargo test -p flinkops-tls --test https -- --ignored --nocapture`

#[test]
#[ignore = "需要外网；本地手动跑：cargo test -p flinkops-tls --test https -- --ignored"]
fn https_handshake_with_rustcrypto_provider() {
    flinkops_tls::install();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("client");
        let resp = client
            .get("https://static.crates.io/crates/serde/serde-1.0.229.crate")
            .send()
            .await
            .expect("https request");
        assert!(
            resp.status().is_success() || resp.status().as_u16() == 206,
            "unexpected status {}",
            resp.status()
        );
        let bytes = resp.bytes().await.expect("body");
        assert!(bytes.len() > 1024, "body too small: {}", bytes.len());
        println!("https ok, {} bytes through rustcrypto provider", bytes.len());
    });
}
