//! 纯 Rust 的 TLS 后端。
//!
//! 为什么有这个 crate：`rustls` 默认走 `aws-lc-rs`（C + 汇编），换 `ring` 也是 C；
//! 两者都会让交叉编译（Windows → Linux / musl / arm64）必须先准备目标平台的 C 工具链
//! （cmake、nasm、zig cc / gcc 交叉工具链）。这里把 provider 换成
//! [`rustls_rustcrypto`]：AES-GCM / ChaCha20-Poly1305 / SHA-2 / HMAC / X25519 / ECDSA / RSA
//! 全部来自 RustCrypto，纯 Rust 实现，于是 `cargo build --target <triple>` 不再需要任何 C 编译。
//!
//! 用法：在每个二进制 `main` 的最开头调用一次 [`install`]。`rustls` 的
//! `ClientConfig::builder()` / `hyper-rustls` / `tokio-rustls` 都会用这个进程级 provider。
//!
//! ```no_run
//! flinkops_tls::install();
//! // …再构造 reqwest / kube client
//! ```

use std::sync::Once;

static INSTALL: Once = Once::new();

/// 安装进程级 [`rustls::crypto::CryptoProvider`]（幂等，重复调用安全）。
///
/// 必须在创建任何 TLS 客户端之前调用：`rustls` 在没有 provider 时会 panic
/// （"no process-level CryptoProvider available"）。
pub fn install() {
    INSTALL.call_once(|| {
        let provider = rustls_rustcrypto::provider();
        match rustls::crypto::CryptoProvider::install_default(provider) {
            Ok(()) => tracing::debug!("rustls provider installed: rustcrypto (pure rust)"),
            // 已经有别的 provider 抢先装了（例如测试里），保持不动
            Err(_) => tracing::debug!("rustls provider already installed, keeping it"),
        }
    });
}

/// 直接拿到 provider，供需要自己组装 `ClientConfig` 的调用方使用。
pub fn provider() -> rustls::crypto::CryptoProvider {
    rustls_rustcrypto::provider()
}
