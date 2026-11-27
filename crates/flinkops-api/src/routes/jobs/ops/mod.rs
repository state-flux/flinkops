//! 作业变更运维：镜像 / 并行度 / Ingress / REST 暴露 / autoscaler / 蓝绿。
//!
//! 638 行时按"是否触碰 Service/Ingress"再拆了一次，路由仍按 `jobs::xxx` 引用。

mod autoscaler;
mod bluegreen;
mod ingress;
mod ops_actions;

pub use autoscaler::AutoscalerBody;
pub use bluegreen::*;
pub use ingress::*;
pub use ops_actions::*;
