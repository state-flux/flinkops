# @flinkops/web

Vue 3 + TypeScript + Tailwind v4 + shadcn-vue 的 SPA，构建产物由 `flinkops-api`（axum）托管。

## 开发

```bash
cd apps/web
npm install
npm run dev          # http://127.0.0.1:5173 ，/api 与 /proxy 自动代理到 :8080
```

先把后端跑起来：

```bash
cargo run -p flinkops-api --bin flinkops
```

## 构建 & 部署

```bash
npm run build                 # 产出 apps/web/dist
# 让 axum 指向它：
FLINKOPS_WEB_DIR=./apps/web/dist cargo run -p flinkops-api --bin flinkops
```

生产镜像里把 `dist` 拷到 `/app/web` 即可（见 `deploy/Dockerfile`）。

## 与设计稿的关系

`web/*.html` 是 22 页设计稿（假数据），这里是它的工程化版本。迁移顺序：
总览 → 作业列表 → 作业详情 → 会话集群 → 设置 → 其余。

设计令牌已从 `web/assets/app.css` 搬到 `src/styles.css` 的 `@theme`，
所以用 Tailwind 类名（`bg-surface`、`text-fg-2`、`border-line-2`）写出来的
视觉和设计稿一致。

## 类型安全

后端 Rust DTO 用 `ts-rs` 导出 TS 类型（下一步接入）：

```rust
// crates/flinkops-types/src/lib.rs
#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../apps/web/src/api/types.ts")]
pub struct JobSummary { /* ... */ }
```

导出后删掉 `src/api/client.ts` 里手写的 `JobSummary` / `SessionCluster`，
改为 `import type { JobSummary } from "./types"` —— 前后端就只有一份定义。