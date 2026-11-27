//! FlinkOps 控制面（axum）—— 模块划分：
//!
//! ```text
//! main.rs        启动装配（tracing / store / clusters / router / serve）
//! config.rs      环境变量 → Config
//! state.rs       AppState（跨 handler 共享）+ 会话 + 审计
//! error.rs       领域错误 → HTTP 响应
//! routes/        每个资源一个模块（system/auth/jobs/profiles/audit/sessions/proxy）
//! ```
//!
//! 新增接口请按 docs/api-spec.md 的「新增接口五步」来，保持结构一致。
pub mod config;
pub mod error;
pub mod extract;
pub mod mcp_guard;
pub mod routes;
pub mod state;
pub mod totp;

pub use config::Config;
pub use state::AppState;
