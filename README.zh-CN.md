# FlinkOps

[English](README.md) | **简体中文**

Flink Kubernetes Operator 的控制台：**单个 Rust 二进制**（axum）托管 **Vue 3 SPA**，元数据存 SQLite / PostgreSQL / MySQL（由 `FLINKOPS_STORE` 决定；`sqlx::Any` 不改写占位符，平台在发送前按方言把 `?` 改写成 `$1`），指标不落库。

> 分层与接口规范见 [`docs/backend-conventions.md`](docs/backend-conventions.md) 与 [`docs/api-spec.md`](docs/api-spec.md)。
> 新增接口请先改文档再改代码，避免文档与实现各走一套。

---

## 界面预览

> 演示环境截图；作业名、镜像、集群与存储路径均已脱敏。

| | |
|:--:|:--:|
| ![运行总览](docs/screenshots/dashboard.png) | ![作业管理](docs/screenshots/jobs.png) |
| **运行总览**：KPI、资源水位、状态分布，一眼看出谁需要关注 | **作业管理**：51 个资源实时同步，吞吐 / 背压 / 槽位 / 资源一屏可达 |
| ![作业详情](docs/screenshots/job-detail.png) | ![监控大盘](docs/screenshots/metrics.png) |
| **作业详情**：生命周期动作、Checkpoint、诊断入口、配置与 YAML 同页 | **监控大盘**：Prometheus 与 Flink REST 双源，吞吐、水位线延迟、Checkpoint 时长 |
| ![检查点与快照](docs/screenshots/savepoints.png) | ![CRD 编辑器](docs/screenshots/editor.png) |
| **检查点与快照**：快照列表与按作业分布，一键从快照恢复 | **CRD 编辑器**：服务端校验 + 与集群 diff，保存即落地（resourceVersion CAS） |

## 目录

```
crates/                        Rust（后端）
├─ flinkops-types/             DTO + CRD 字段提取 + 错误（不依赖 kube/axum）
├─ flinkops-tls/               纯 Rust TLS provider（rustls + RustCrypto），交叉编译不再需要目标平台 C 工具链
├─ flinkops-core/              纯逻辑：基线合并 / 校验 / diff / 健康度
├─ flinkops-store/             sqlx::Any → SQLite / PostgreSQL（方言层重写占位符）
├─ flinkops-k8s/               kube 动态客户端（flink.apache.org/v1beta1）
├─ flinkops-flink-rest/        JobManager REST（指标/日志/背压/profiler）
├─ flinkops-api/               axum 控制面
│  └─ src/routes/              system / auth / jobs / profiles / audit / sessions / proxy / editor
└─ flinkops-cli/               flinkopsctl（doctor / jobs / export）

apps/web/                      Vue 3 + TS + Tailwind v4 + shadcn-vue + CodeMirror 6
docs/
├─ api-spec.md                 ★ 接口契约（含新增接口五步）
├─ backend-conventions.md      ★ 分层/命名/数据源规则
├─ data-sources.md             ★ 哪类数据只能从哪来（最容易写错的一页）
├─ mcp-spec.md                 MCP 协议与工具面（Token/范围/审批）
└─ screenshots/                README 用截图

deploy/                        Dockerfile（前端构建 + distroless）+ Helm chart
```

`main.rs` 只做接线（89 行）：`tracing → Config → Store → Clusters → AppState → router → serve`。

---

## 快速开始

### 本地（连已有集群）

```bash
export FLINKOPS_CLUSTERS="prod=$HOME/.kube/config"     # 或 id=incluster
export FLINKOPS_STORE="sqlite://./data/flinkops.db?mode=rwc"
export FLINKOPS_WEB_DIR="./apps/web/dist"               # 先 npm run build

cargo run -p flinkops-api --bin flinkops                # http://127.0.0.1:8080  admin/admin
```

Windows 上 SQLite 路径必须用**三斜杠 + 正斜杠**（否则会被当成 host 解析）：`sqlite:///D:/work/flinkops/data/flinkops.db?mode=rwc`。

### IDE 里直接点运行（零配置）

把 kubeconfig 放到 `.secrets/kubeconfig.yaml`，然后直接 run 即可，不需要设任何环境变量：

1. 若存在 `.env` 会自动加载（不覆盖已有变量）
2. 未设 `FLINKOPS_CLUSTERS` 时，自动使用 `.secrets/kubeconfig.yaml`；没有才回退 `incluster`
3. 相对路径（web 目录、sqlite DSN）会从当前目录**向上找 workspace 根**，所以从 `crates/flinkops-api` 目录启动也正常

日志会打印最终解析结果，便于确认：

```
INFO starting flinkops store=sqlite:///D:/work/flinkops/data/flinkops.db web=apps/web/dist clusters=1
INFO cluster connected cluster=local
```

### 前端开发

```bash
cd apps/web && npm install && npm run dev               # :5173，/api 与 /proxy 自动代理到 :8080
```

### 发行包（Releases）

打 tag 后由 CI 在原生 runner 上编译，同一套布局、附 `SHA256SUMS.txt`：

| 平台 | 产物 | 安装 |
|---|---|---|
| Linux x86_64 / arm64 | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Debian / Ubuntu | `flinkops_<ver>_amd64.deb` / `_arm64.deb` | `dpkg -i`，systemd unit |
| macOS Intel / Apple Silicon | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Windows x86_64 | `.zip`（便携） / `-setup.exe`（安装包） | 解压双击 `flinkops.exe`；安装包免管理员 |
| 源码（任意平台） | `-src.tar.gz` / `-src.zip` | 解压后 `cargo build --release` |

每个包里是 `bin/{flinkops,flinkopsctl,flinkops-mcp}` + `web/` + 配置样例与 systemd unit。
发版细节（版本对齐、产物矩阵、打包器参数、回滚）见 [`docs/release.md`](docs/release.md)。

### Docker

```bash
docker run -d --name flinkops -p 8080:8080 -v flinkops-data:/data \
  -v ~/.kube/config:/etc/flinkops/kubeconfig:ro \
  -e FLINKOPS_CLUSTERS="prod=/etc/flinkops/kubeconfig" \
  ghcr.io/state-flux/flinkops:0.1.0
```

自己构建：`docker build -f deploy/Dockerfile -t flinkops:dev .`

### Kubernetes / k3s

用发布出来的 chart（OCI，版本号与镜像一致）：

```bash
helm install flinkops oci://ghcr.io/state-flux/charts/flinkops \
  --version 0.1.0 -n flinkops --create-namespace \
  --set store.storageClass=local-path \
  --set ingress.enabled=true --set ingress.className=traefik   # 标准 K8s 用 nginx
```

从仓库里装：`helm install flinkops deploy/helm/flinkops -n flinkops --create-namespace`

> 只有把平台部署**在集群内**（`clusters: "local=incluster"`）才能用 `ClusterIP` 访问作业
> REST；从集群外接入必须保留 `NodePort`（或 port-forward）。

---

## 元数据后端

`FLINKOPS_STORE` 决定元数据落在哪，三种后端都实测过（`store_smoke` 覆盖 migrate 幂等、写入、带条件查询、upsert）：

| 后端 | DSN 示例 | 说明 |
|---|---|---|
| SQLite | `sqlite:///abs/path/flinkops.db?mode=rwc` | 默认，零依赖；单写者，建议单副本 |
| PostgreSQL | `postgres://user:pass@host:5432/flinkops` | 多副本共享同一份元数据时的首选 |
| MySQL | `mysql://user:pass@host:3306/flinkops?charset=utf8mb4` | 可用；长文本列落 `MEDIUMTEXT`，读取走 bytes→utf8 |

方言差异集中在 `crates/flinkops-store/src/dialect.rs`：占位符（Postgres 用 `$1`）、建表（MySQL 的 TEXT 不能做主键/不能带默认值）、
以及 MySQL 上 `TEXT` 被 `sqlx::Any` 报成 BLOB 的解码兼容。

---

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `FLINKOPS_BIND` | `0.0.0.0:8080` | 监听地址 |
| `FLINKOPS_CLUSTERS_DIR` | SQLite 同级的 `clusters/` | 粘贴方式接入时 kubeconfig 落盘位置（目录 0700 / 文件 0600） |
| `FLINKOPS_STORE` | `sqlite://./data/flinkops.db?mode=rwc` | 也支持 `postgres://…`、`mysql://…`（见下方“元数据后端”） |
| `FLINKOPS_COOKIE_SECURE` | 不设=自动 | `true`/`false`；自动时按 `X-Forwarded-Proto` 决定 Cookie 是否加 `Secure` |
| `FLINKOPS_CLUSTERS` | `local=incluster` | `id=incluster` 或 `id=/path/kubeconfig`，逗号分隔 |
| `FLINKOPS_WEB_DIR` | `./apps/web/dist` | 前端构建产物目录 |
| `FLINKOPS_READONLY` | `false` | 平台级只读（拒绝写 API） |
| `FLINKOPS_PROMETHEUS_URL` | 空 | 留空 = 只有 REST 实时数据，无历史 |
| `FLINKOPS_ADMIN_USER` / `_PASSWORD` | `admin` / `admin` | 生产请用 Secret |
| `FLINKOPS_AUDIT_RETENTION_DAYS` | `90` | 审计保留天数 |
| `FLINKOPS_DB_MAX_MB` | `512` | DB 体积告警阈值 |

kubeconfig 放 `.secrets/`（已在 `.gitignore`），不要提交进仓库。

---

## 页面（22 个）

每个页面都在 `apps/web/src/views/` 下，**拿不到后端接口的地方给"诚实空态"**（说明缺什么接口），而不是塞假数字。

| 页面 | Vue | 路由 | 数据 |
|---|---|---|---|
| index | Dashboard | `/` | `/api/jobs` + `/api/audit` |
| jobs | Jobs | `/jobs` | `/api/jobs`（含同步） |
| job-detail | JobDetail | `/jobs/:c/:ns/:n` | 详情 + 9 个 Tab（REST 指标/YAML/日志） |
| sessions | Sessions | `/sessions` | `/api/sessions` |
| clusters | Clusters | `/clusters` | `/api/me` + 作业聚合 |
| cluster-detail | ClusterDetail | `/clusters/:id` | 作业聚合 |
| savepoints | Savepoints | `/savepoints` | 详情 + `POST …/savepoint` |
| metrics | Metrics | `/metrics` | REST 实时（无 Prometheus 时说明原因） |
| alerts | Alerts | `/alerts` | 由作业状态派生（规则引擎未实现） |
| submit | Submit | `/submit` | `POST /api/render` + `/api/validate` |
| editor | Editor | `/editor/:c?/:ns?/:n?` | `GET …/yaml` + `/api/validate` + `/api/diff` |
| sql | Sql | `/sql` | `/api/sessions`（Gateway 未接） |
| templates | Templates | `/templates` | 从作业派生镜像清单 |
| presets | Presets | `/presets` | `/api/profiles` + `PUT` |
| deploy | Deploy | `/deploy` | 静态运维文档 + 真实接入状态 |
| observability | Observability | `/observability` | 作业清单 + PodMonitor 生成器 |
| namespaces | Namespaces | `/namespaces` | 作业聚合 |
| users | Users | `/users` | `/api/me` |
| audit | Audit | `/audit` | `/api/audit` |
| settings | Settings | `/settings` | `/api/me` + 审计 + 存储说明 |
| flink-ui | FlinkUi | `/ui/:c/:ns/:n` | iframe → `/proxy/...`（只读） |
| login | Login | `/login` | 账号密码登录 + 会话 Cookie |

截图在 `docs/screenshots/`（dashboard / jobs / job-detail / metrics / savepoints / editor / login）。

## 实现状态

| 部分 | 状态 |
|---|---|
| 后端分层（9 个模块） | ✅ `main.rs` 只做接线；`routes/jobs.rs` 是最大单文件（作业 22 个端点在同一个领域里，尚未拆分） |
| 作业 22 个端点（列表/详情/指标/日志/启停/重启/savepoint/恢复/扩容/升级/Ingress/autoscaler/背压/火焰图） | ✅ 真实集群验证 |
| 只读反代（含 query 判定、WebSocket 透传） | ✅ 写操作全部 403；`/ws` 101 握手通过 |
| 编辑器支撑 `validate` / `diff` / `yaml` / `render` | ✅ 服务端解析 YAML，与提交同规则 |
| 事件 / 异常 / 快照 | ✅ `…/events`、`…/exceptions`、`GET/DELETE /api/snapshots` |
| 审计（保留期清理 + 体积检查） | ✅ |
| 前端 8 个路由（总览/作业/详情/会话/编辑器/设置/登录） | ✅ 已接真实 API |
| 共享存储 | ✅ SQLite（PVC / 单实例）· PostgreSQL · MySQL（多副本共享同一份元数据） |
| 登录 | ✅ 本地账号 argon2id + 登录限流(5次/10分钟锁5分钟) + CSRF 双提交 + 可选 TOTP；会话落 SQLite（重启不掉），过期自动清理 |
| 会话集群创建 | ✅ `POST /api/sessions`（基线渲染 + `mode=session` + 校验 + dryRun） |
| WebSocket 反代 | ⚠️ 返回 501，Flink UI 降级轮询 |
| 提交作业 | ✅ `POST /api/jobs`（校验 → 重名 → apply → 审计，支持 `?dryRun=true` 预检） |
| **MCP（AI 客户端接入）** | ✅ `flinkops-mcp` 17 个工具 + Token/范围/QPS/一次性审批，见 [`docs/mcp-spec.md`](docs/mcp-spec.md)（HTTP transport 待补） |

## MCP：让 AI 客户端安全地管 Flink

机器不拿集群凭据，只拿平台签的 Token；平台侧四道闸门（scope → 资源范围 → QPS → 一次性审批）后才落到 K8s。

```bash
# 1) 网页 →「MCP 与审批」→ 创建 Token（限定 ns=flink，scopes=read,operate）
# 2) 接进任意 MCP 客户端
FLINKOPS_URL=http://127.0.0.1:8080 \
FLINKOPS_MCP_TOKEN=fko_pat_xxxxxxxx_yyyy \
  flinkops-mcp --stdio
```

```json
{
  "mcpServers": {
    "flinkops": {
      "command": "flinkops-mcp",
      "args": ["--stdio"],
      "env": { "FLINKOPS_URL": "http://127.0.0.1:8080", "FLINKOPS_MCP_TOKEN": "fko_pat_..." }
    }
  }
}
```

高危操作（挂起 / 扩缩容 / 恢复 / 换镜像 / 删除）**默认打不出去**：第一次调用返回 `approval_required:<id>`，
人工在「MCP 与审批」里点了批准，客户端带 `approvalToken` 重试才生效，且**一次有效**。


## 真实集群验证结果（Flink 1.15.4 + 老版 operator）

- 拉取 **47 个 FlinkDeployment**（`flink` 命名空间），状态 `RUNNING`
- 健康度 **44 green / 3 yellow**（该版本 operator 不写 `status.conditions`，改用 `lifecycleState=STABLE` + `jobManagerReady` 兜底）
- 时间戳：真实值 `1785146781543`（毫秒字符串）→ `2026-07-27T10:06:21Z`
- 校验对真实 YAML 给出：`REST_NODEPORT`(warn)、`STATE_BACKEND_LEGACY_KEY`(info)
- 反代读 → 502（JobManager Service 为集群内 DNS，办公网不可达，属预期降级）

---

## 测试

```bash
cargo test --workspace                              # 29 个单测（CRD 提取 / 合并幂等 / 校验 / diff / 反代白名单 / 配置解析 / 路径归一化 / spec.metadata 提升 / MCP 脱敏）
cargo clippy --workspace --all-targets -- -D warnings
cd apps/web && npm run build                        # vue-tsc + vite（gzip ~50KB，CodeMirror 单独分包）
cargo deny check licenses                           # 白名单见 deny.toml
```

## 许可

Apache-2.0。发行物不含 JVM、不含 Flink 发行包、不含 GPL 连接器。
"Apache Flink" 是 ASF 商标；本项目不暗示官方背书。
---

## 上线前走查清单（真机顺序）

按日常运维顺序走一遍，每步都要看**真实数据**而不是"没报错"：

| # | 步骤 | 期望 |
|---|---|---|
| 1 | 作业管理：列表加载 | 有真实作业；表头可排序；分页正确；行内能被点 |
| 2 | 进作业详情 | Conditions/基本信息与 `kubectl get fd` 一致 |
| 3 | 指标页签 | `source: flink-rest` 且 `degraded: null`；有 checkpoint 计数与吞吐 |
| 4 | 触发 Savepoint | 返回 snapshot 名；「全部快照」出现该条（或提示 CRD 缺失） |
| 5 | 检查点与快照 | 列表能按状态筛；清理只删 FAILED |
| 6 | 告警中心 | 待处理有数据；确认后转入"已确认"；规则可增删 |
| 7 | 审计日志 | 上面每一步都有对应 action（job.savepoint / alert.ack / ui.open…） |
| 8 | 打开 Flink UI | 反代页面加载完整；Cancel 被拦成 403 并记 `proxy.blocked` |
| 9 | 监控大盘 | `flink_*` 指标数 > 0；历史曲线有数据点 |
| 10 | 设置 | 诊断包可下载；VACUUM 有 before/after 字节数 |

## 已知限制

- **动态接入的集群**：持久化在库，但 env 里的集群优先级更高（同 id 不覆盖）
- **session cluster**：`mode` 必须是 `native` 且**无 `job` 段**（旧文档的 `mode=session` 枚举已被 operator 移除）
- **模板生成**：白名单脱敏，`podTemplate` 整段丢弃（那里常含 AK/SK），因此模板不含 initContainer
- **用户体系**：单账号（`FLINKOPS_ADMIN_USER/PASSWORD`），不做多租户