# Changelog

All notable changes to FlinkOps are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-27

首个公开版本：面向 Apache Flink on Kubernetes 的运维控制台，单个 Rust 二进制同时托管 Vue 3 前端。

### Added

#### 作业生命周期
- `FlinkDeployment` / `FlinkSessionJob` / `FlinkStateSnapshot` 全生命周期操作：
  启动、挂起（suspend）、重启、触发 savepoint、从 savepoint 恢复、并行度伸缩、
  镜像升级（`upgradeMode`）、Ingress 开关、自动伸缩（autoscaler）。
- 所有动作经由 CRD 写入（`savepointTriggerNonce` / `restartNonce`），并逐条落审计行。
- 作业详情页：checkpoint、diagnostics、运行配置、CRD YAML 一屏可查。

#### 只读 Flink Web UI 反向代理
- 隧道化 Flink Web UI 的 HTML、静态资源与 `/ws`，多集群多作业互不抢端口。
- 在代理层拒绝 cancel / stop / savepoint / rescale 等写操作并记审计（`403`）。

#### 可观测性
- Prometheus 指标穿透，带指标名白名单；吞吐、watermark 延迟、checkpoint 时长曲线。
- 每算子热点表、背压热力图。
- 实时同步资源视图（吞吐 / 背压 / slots / 资源配置）。

#### 告警
- 阈值规则每 60s 评估；「NodePort 暴露」仅在控制台自身运行于集群内时才会触发。

#### MCP
- `flinkops-mcp` 向 AI 客户端暴露 17 个工具，权限链路为
  scope → 资源白名单 → QPS → 一次性人工确认，规格见 `docs/mcp-spec.md`。
- 支持 stdio 与 Streamable HTTP 两种传输。

#### 多集群与存储
- `FLINKOPS_CLUSTERS` 支持 `id=incluster` 与 `id=/path/kubeconfig`，可并存多个集群。
- `FLINKOPS_STORE` 支持 SQLite / PostgreSQL / MySQL（`sqlx::Any`，按方言改写占位符）。

#### 发行与部署
- 单文件发行包：`flinkops`（控制面 + Web UI）、`flinkopsctl`（CLI）、`flinkops-mcp`。
- 多平台产物（每个都带同名 `.sha256`）：
  - Linux amd64 / arm64：`.tar.gz`、`.deb`（systemd unit）、`.AppImage`（单文件免安装）；
  - macOS Intel / Apple Silicon：`.tar.gz`；
  - Windows x86_64：`.zip`（解压双击即用）、`setup.exe`（按用户安装，免管理员）；
  - 源码：`src.tar.gz` / `src.zip`（`git ls-files` 清单）；
  - 汇总 `SHA256SUMS.txt` + 每个包内的 `build-info.json`。
- 多架构容器镜像 `ghcr.io/state-flux/flinkops`（`linux/amd64` + `linux/arm64`，distroless）。
- Helm chart 推送至 `ghcr.io/state-flux/charts`（OCI）。

### Changed
- 便携包与安装包改为「解压/安装即可用」：二进制放在包根目录，附带零配置 `.env`；
  `.env` 在读取任何配置前加载，相对路径以 exe 所在目录解析（`web/`、`data/`）。
- 默认监听地址从 `0.0.0.0:8080` 改为 `127.0.0.1:8080`（Dockerfile 与 Helm chart 显式指定 `0.0.0.0`，不受影响）。
- Windows 安装包改为**按当前用户安装**（`%LOCALAPPDATA%\Programs\FlinkOps`），不再需要管理员权限与 UAC。
- 启动横幅改为 ASCII 并打印访问地址，`FLINKOPS_OPEN_BROWSER=1` 时自动打开浏览器。

### Changed (TLS / 交叉编译)
- TLS 后端换成**纯 Rust**：`rustls` + `rustls-rustcrypto`（AES-GCM / ChaCha20-Poly1305 / SHA-2 /
  HMAC / X25519 / ECDSA / RSA 全部来自 RustCrypto），移除 `aws-lc-rs` 与 `ring` 的 C/汇编依赖；
  `reqwest` 改用 `rustls-no-provider`，`kube` 关闭默认 `ring` 特性并启用 `rustls-tls` + `webpki-roots`。
  结果：依赖树里只剩 `libsqlite3-sys`（bundled SQLite）一处 C 代码。
- 新增 `crates/flinkops-tls`：`flinkops_tls::install()` 安装进程级 provider，三个二进制启动时调用一次。
- 最低 Rust 版本抬到 **1.87**（`rustls-rustcrypto` 要求），CI 固定工具链同步到 1.87。
- 因为不再需要目标平台 C 工具链编译加密库，`cargo zigbuild --target x86_64-unknown-linux-musl`
  这类交叉编译变得可用（bundled SQLite 由 `zig cc` 编译，不再需要 cmake/nasm）。

### Security
- 会话 cookie 与 CSRF 防护、TOTP 双因子、按角色的接口鉴权。
- MCP 工具默认需要一次性人工确认；敏感字段在返回前脱敏。

### Known issues
- Windows `setup.exe` 与全部压缩包、`.deb` 均**未做代码签名**，首次运行会出现
  SmartScreen / Gatekeeper 提示；请用 `SHA256SUMS.txt` 校验后再运行。
- 暂未提供 `.rpm` 与 macOS 公证后的 `.dmg`。
- 默认管理员为 `admin/admin`，首次登录后必须改密（生产环境请通过环境变量预置）。
- 前端静态资源随包分发；从源码运行时需先 `cd apps/web && npm ci && npm run build`。

[Unreleased]: https://github.com/state-flux/flinkops/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/state-flux/flinkops/releases/tag/v0.1.0
