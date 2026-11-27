# 参与 FlinkOps 开发

[English](CONTRIBUTING.md) | **简体中文**

感谢愿意花时间贡献。这个项目直接操作生产环境的 Kubernetes 集群，所以对"明显正确"的要求
比一般项目高一些。

## 基本原则

1. **先改文档，再改代码。** 任何接口或行为变化，先改 `docs/api-spec.md`（或
   `docs/mcp-spec.md`）。文档和代码不一致时，以文档为准，代码算 bug。
2. **不造假数据。** UI 上的每个数字都必须来自 API；拿不到就显示"诚实空态"并写清楚
   缺什么接口，不允许塞示例数据兜底。
3. **写操作必须落到 CRD。** UI 层不能绕过 CRD 直接改集群；生命周期动作要落在
   `FlinkDeployment` / `FlinkStateSnapshot` 上，并写一条审计。
4. **一个 PR 只做一件事。** 重构和行为变更分开提。

## 本地起环境

```bash
cargo build --workspace
# 前端统一用 npm（仓库里有 package-lock.json，CI 走 npm ci；不要混用 pnpm/yarn）
cd apps/web && npm install && npm run build && cd ../..
cp .env.example .env          # 可选；开发容器里默认值就能跑
cargo run -p flinkops-api --bin flinkops
```

没有集群也能把界面跑起来；要真实数据就把 `FLINKOPS_CLUSTERS` 指向 kubeconfig，或用
`kind` / `k3d` 装一个 Flink Kubernetes Operator。

## 提交前必跑

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/web && npx vue-tsc -b && npm run build
```

CI 就是这四条。`clippy -D warnings` 不过的 PR 不会再往下看。

## 提交信息

用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/)：

```
feat(mcp): 新增 flinkops_scale 工具，绑定审批参数哈希
fix(metrics): 吞吐只统计 Source 算子
docs(api): 补充 /api/approvals/stream
```

scope 用 crate 或领域名（`api` / `web` / `k8s` / `store` / `mcp` / `metrics` / `docs`）。

## 新增接口的五步

1. `docs/api-spec.md` —— 请求、响应、错误码、审计 action。
2. `crates/flinkops-api/src/routes/<resource>.rs` —— handler，先 `require_user`，
   涉及单作业的路由再加 `mcp_guard::check_job_scope` / `gate_write`。
3. `crates/flinkops-api/src/routes/mod.rs` —— 注册路由。
4. `apps/web/src/api/client.ts` —— 加类型化客户端函数。
5. 有分支逻辑的加单测；用户可见的功能在 README 状态表补一行。

## 新增 MCP 工具

按 [`docs/mcp-spec.md`](docs/mcp-spec.md) §3 来。只读工具放 `tools.rs`，名字必须
`flinkops_` 前缀；任何改集群状态的工具都要有 scope、资源范围校验和审批闸门——**闸门在
平台侧，不在 MCP 二进制里**，这样即使有人绕过 MCP 直接打 REST，破坏力也一样被卡住。

## 代码风格

- Rust：`cargo fmt`；请求路径上不允许 `unwrap()`。
- Vue：`<script setup lang="ts">`，不用 `any`，组件放 `components/`，一个路由一个 view。
- 注释写"为什么"，用文件本身的语言（中英都行，但别在一个文件里混）。

## 报 bug

提 issue 请带上：FlinkOps 版本、Flink / operator 版本、集群接入方式（`incluster` 还是
kubeconfig）、完整请求与响应。**不要贴 token、kubeconfig、`last-applied-configuration`
或 Pod 环境变量值。**

安全问题见 [SECURITY.zh-CN.md](SECURITY.zh-CN.md)。
