# FlinkOps docs / 文档索引

**English** | [简体中文](#简体中文)

## English

| Document | Language | What it covers |
|---|---|---|
| [`backend-conventions.en.md`](backend-conventions.en.md) | EN · [中文](backend-conventions.md) | Layering, naming, error handling, handler skeleton, where data comes from |
| [`data-sources.en.md`](data-sources.en.md) | EN · [中文](data-sources.md) | CRD vs Flink REST vs Prometheus, REST address resolution, degradation, WebSocket proxy, PodMonitor setup |
| [`api-spec.en.md`](api-spec.en.md) | EN · [中文](api-spec.md) | HTTP contract for every endpoint, plus the five-step recipe for adding one |
| [`mcp-spec.en.md`](mcp-spec.en.md) | EN · [中文](mcp-spec.md) | MCP contract: token format, scopes, resource allow-list, one-shot approvals, 17 tools |
| [`release.en.md`](release.en.md) | EN · [中文](release.md) | Release runbook: version bump, artifact matrix, packager flags, installers, verification |

Convention: `*.md` is Chinese, `*.en.md` is English, and both sides carry a language
switcher at the top. Keep the two versions in sync when you change a contract — a PR that
updates only one of them is incomplete. See [CONTRIBUTING](../CONTRIBUTING.md).

## 简体中文

| 文档 | 语言 | 内容 |
|---|---|---|
| [`backend-conventions.md`](backend-conventions.md) | 中文 · [EN](backend-conventions.en.md) | 分层、命名、错误处理、handler 骨架、数据来源 |
| [`data-sources.md`](data-sources.md) | 中文 · [EN](data-sources.en.md) | CRD / Flink REST / Prometheus 的分工、REST 地址选路、降级语义、WS 反代、PodMonitor 配置 |
| [`api-spec.md`](api-spec.md) | 中文 · [EN](api-spec.en.md) | 每个接口的 HTTP 契约 + 新增接口五步 |
| [`mcp-spec.md`](mcp-spec.md) | 中文 · [EN](mcp-spec.en.md) | MCP 契约：Token 形态、scope、资源范围、一次性审批、17 个工具 |
| [`release.md`](release.md) | 中文 · [EN](release.en.md) | 发版手册：版本对齐、产物矩阵、打包器参数、安装包行为、发布后校验 |

约定：`*.md` 是中文、`*.en.md` 是英文，两侧顶部都有语言切换。**改契约时两份一起改**——
只改一边的 PR 视为未完成。见 [CONTRIBUTING.zh-CN.md](../CONTRIBUTING.zh-CN.md)。

## 截图 / Screenshots

`docs/screenshots/` — dashboard, jobs, login.
