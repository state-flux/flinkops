//! 路由层中间件集合：CSRF / RBAC / 写操作单飞 / 失败审计 / 错误信封。
//!
//! 拆出来的原因：`routes/mod.rs` 之前同时承担路由表 + 5 个中间件 + 工具函数（664 行），
//! 改路由和改中间件的人会互相踩。现在 mod.rs 只留装配。

mod action;
mod audit;
mod csrf;
mod error;
mod rbac;

pub(crate) use action::action_guard;
pub(crate) use audit::audit_failures;
pub(crate) use csrf::csrf_guard;
pub(crate) use error::{api_not_found, normalize_rejection};
pub(crate) use rbac::rbac_guard;
