# MCP 接入规范（v1 · 契约）

**简体中文** | [English](mcp-spec.en.md)

> 这份文档是**契约**：先改这里，再改代码。MCP server 与平台接口的行为以本文档为准。
> 上游约定见 [`api-spec.md`](api-spec.md)（错误结构、审计 action 命名、camelCase）与 [`backend-conventions.md`](backend-conventions.md)（分层、命名、禁止吞错）。

---

## 0. 范围与非目标

**做**：把 FlinkOps 的**运维能力**以 MCP 工具形式暴露给 AI 客户端（Claude Desktop / Cursor / 自建 agent），并在服务端把破坏力关进笼子。

**不做**：

| 不做 | 原因 |
|---|---|
| 让 MCP 进程直接持 kubeconfig | 一旦泄露 = 整个集群可删；所有请求必须过平台鉴权与审计 |
| 复用登录 Cookie 做机器认证 | 浏览器会话不该给机器用；需要可吊销、可限范围的 Token |
| 暴露 Flink UI 任意反代路径 | 万能 proxy 等于绕过所有结构化约束 |
| 暴露「提交常驻作业 / 新建集群」 | 边界过大；由人在网页端做 |
| 暴露任意 PromQL | 会打到集群里别的系统；只给白名单前缀 |

---

## 1. 进程与传输

```
MCP host（Claude/Cursor/agent）
        │ stdio 或 Streamable HTTP
        ▼
flinkops-mcp（独立二进制，crates/flinkops-mcp）
        │ HTTP + Bearer token
        ▼
flinkops API（axum，唯一鉴权与审计出口）
```

**硬规则**

1. `flinkops-mcp` **只依赖平台 HTTP API**，不链接 `flinkops-k8s`，不读 kubeconfig。
2. 传输两种，默认 stdio：

| 传输 | 开关 | 说明 |
|---|---|---|
| stdio | 默认 | 本地/桌面；日志一律走 stderr（stdout 留给协议帧） |
| Streamable HTTP | `FLINKOPS_MCP_HTTP=1` + `FLINKOPS_MCP_TOKEN` | 远程部署；必须配 `Host`/`Origin` 白名单 |

3. **fail-closed**：平台不可达时，每个工具返回 `unavailable` + 原因；**禁止返回空列表**（空列表会被模型读成"没有作业"）。
4. 环境变量：

| 变量 | 默认 | 说明 |
|---|---|---|
| `FLINKOPS_API_URL` | `http://127.0.0.1:8080` | 平台地址 |
| `FLINKOPS_MCP_TOKEN` | 空（必填） | API Token，见 §2 |
| `FLINKOPS_MCP_HTTP` | `0` | 1 = 开 Streamable HTTP |
| `FLINKOPS_MCP_BIND` | `127.0.0.1:8765` | HTTP 监听地址 |
| `FLINKOPS_MCP_ALLOWED_HOSTS` | 空 | 逗号分隔；空则只允许 bind 地址 |
| `FLINKOPS_MCP_ALLOWED_ORIGINS` | 空 | 浏览器 Origin 白名单；空则只允许无 Origin |

---

## 2. 认证：API Token（PAT）

### 2.1 形态

```
fko_pat_<id>_<secret>
     │      │     └─ 32 字节随机（base64url），只在创建时返回一次
     │      └─ 8 位短 id，用于展示/吊销
     └─ 固定前缀，便于日志脱敏与泄露扫描
```

服务端只存 `sha256(secret)` 与 `id`，**永不回显 secret**。

### 2.2 表结构（`flinkops-store/src/lib.rs` 的 `MIGRATIONS` 常量）

```sql
create table mcp_token (
  id            text primary key,          -- 8 位短 id
  name          text not null,
  secret_hash   text not null,             -- sha256(secret) hex
  scopes        text not null,             -- json: ["read","operate"]
  allow_jobs    text not null,             -- json: ["flink/etl-*", "flink/order-sync"]
  allow_ns      text not null,             -- json: ["flink"]
  qps           integer not null default 5,
  expires_at    text,                      -- RFC3339；null = 不过期（需 admin）
  created_at    text not null,
  last_used_at  text,
  revoked_at    text
);
```

### 2.3 scope 与资源匹配

| scope | 含义 |
|---|---|
| `read` | 只读工具 |
| `operate` | 生命周期动作（savepoint/restart/suspend/resume/scale/restore/升级镜像） |
| `admin` | 删除作业、改基线/设置/告警规则 |

资源匹配（**默认最小，空即无权**）：

- `allow_ns`：`["flink"]` 或 `["flink-*"]`（glob）
- `allow_jobs`：`"<namespace>/<name>"` 形式，支持 `*`；跨 namespace 必须显式列出

### 2.4 校验顺序（每次都按此顺序，任一失败即停）

```
1. token 存在且未 revoked / 未过期        → invalid_token
2. secret_hash 匹配                        → invalid_token
3. 工具所需 scope ⊆ token.scopes           → denied（message 说明缺哪个）
4. 目标 namespace ∈ allow_ns               → denied
5. 目标 job ∈ allow_jobs（或 ns 级通配）   → denied
6. 需要审批的工具 → 检查 approval_token     → approval_required / approval_invalid
7. 限流（QPS / 并发 / 批量上限）           → rate_limited
8. 执行；写审计 mcp.call
```

> **审批之后必须重新执行 3–5（重新鉴权）**，不能"批过就放行"。

---

## 3. 工具目录

命名前缀统一 `flinkops_`。所有工具入参 camelCase，返回 JSON。

### 3.1 只读（scope: `read`）

| 工具 | 入参 | 返回 | 备注 |
|---|---|---|---|
| `flinkops_list_jobs` | `cluster?`, `namespace?`, `state?`, `health?`, `limit?`(默认 20，上限 50)、`cursor?` | `{items:[JobSummary], nextCursor?}` | 复用 `GET /api/jobs` |
| `flinkops_get_job` | `cluster`,`namespace`,`name` | `{summary, checkpoint, savepoint}` | 不含 events/logs |
| `flinkops_job_metrics` | 同上 | `{jobId, values, source, sampledAt, degraded}` | `degraded!=null` 时必须原样回传，不许当 0 |
| `flinkops_job_exceptions` | 同上 | `{rootCause?, exceptions:[...]}` | 只回最近 3 条 |
| `flinkops_job_events` | 同上, `limit?`(默认 20) | `{items:[{type,reason,message,lastTimestamp}]}` | |
| `flinkops_job_yaml` | 同上 | `{yaml, redacted:true, dropped:[...]}` | 见 §5 脱敏 |
| `flinkops_list_alerts` | `state?`(`OPEN`\|`ACK`), `severity?` | `{items:[AlertEvent], open}` | |
| `flinkops_list_snapshots` | `cluster?`,`namespace?`,`state?` | `{items:[Snapshot], count, unsupportedClusters?}` | |
| `flinkops_prom_query` | `query`,`rangeSec?`(默认 0=瞬时) | `{series:[...]}` | **白名单前缀**：`flink_`、`kube_node_`、`kube_pod_`、`node_`；其它前缀 → `bad_request` |

### 3.2 生命周期（scope: `operate`）

| 工具 | 入参 | 审批 | 返回 |
|---|---|---|---|
| `flinkops_savepoint` | `cluster`,`namespace`,`name`,`path?` | 否 | `{mode:"cr"\|"rest", snapshot?, degraded, requestId?}` |
| `flinkops_restart` | 同上 | 否 | `{restartNonce}` |
| `flinkops_suspend` | 同上 | **是** | `{state:"suspended"}` |
| `flinkops_resume` | 同上 | 否 | `{state:"running", clearedStale:[...]}` |
| `flinkops_scale` | 同上, `parallelism?`,`taskManagerReplicas?` | **是** | `{applied:{...}}` |
| `flinkops_restore` | 同上, `savepointPath`（**必填**） | **是** | `{ok:true}` |
| `flinkops_upgrade_image` | 同上, `image`, `upgradeMode`（默认并强制 `savepoint`） | **是** | `{ok:true}` |

- `restore` 不接受"最近一次"的隐式语义：AI 必须把路径写出来，人才能审。
- `scale` 未给任何参数 → `bad_request`；给了但超出集群剩余资源 → 返回 `insufficient_resources` + 差额（复用提交页的资源预检口径）。

### 3.3 高危（scope: `admin`，**必须审批 + 二次确认**）

| 工具 | 入参 | 额外要求 |
|---|---|---|
| `flinkops_delete_job` | `cluster`,`namespace`,`name`,`confirmName` | `confirmName` 必须与 `name` 完全一致，否则 `bad_request` |

### 3.4 明确不暴露（写进代码注释，防止后人加）

`kubeconfig` / `Secret` / `Token` / 连接器凭据 / `POST /api/jobs`（提交）/ `POST /api/clusters`（接入）/ `PUT /api/profiles`（基线）/ 任意 `/proxy/*`

---

## 4. 审批流程

```
① AI: flinkops_suspend{job}
② 服务端: 不执行 → 写 approval 行 → 返回
   { "error": { "code": "approval_required",
                "message": "suspend 需要人工审批",
                "approvalId": "ap_7f3c…", "expiresInSec": 300 } }
③ 网页「审批中心」: 展示 tool / 目标 / 参数摘要 / 发起者 token
④ 值班人批准 → 生成一次性 approval_token（只回显一次给操作者）
⑤ AI 重试同一调用，Header: X-FlinkOps-Approval: <token>
⑥ 服务端校验：token 未用、未过期、tool 一致、argsHash 一致 → 执行 → 标记已用
```

**规则**

| 项 | 值 |
|---|---|
| 审批有效期 | 300s |
| token | 一次性；用后立即失效 |
| argsHash | `sha256(canonical_json(args))`，审批 A 却执行 B → `approval_invalid` |
| 谁能批 | 网页登录用户（后续接 RBAC 时限定 `operate` 角色） |
| 超时后 | 状态 `expired`，审计留痕，AI 需重新发起 |

表结构：

```sql
create table mcp_approval (
  id          text primary key,   -- ap_xxx
  tool        text not null,
  args_hash   text not null,
  target      text not null,      -- ns/name
  actor       text not null,      -- mcp:<token name>
  token_id    text not null,
  status      text not null,      -- pending|approved|rejected|expired|used
  created_at  text not null,
  decided_at  text,
  decided_by  text
);
```

---

## 5. 脱敏规则

`flinkops_job_yaml` 与任何回传 YAML/日志的工具，**必须**按此表处理，并在返回里带 `redacted: true` 与 `dropped: [...]`：

| 字段 | 处理 |
|---|---|
| `metadata.annotations."kubectl.kubernetes.io/last-applied-configuration"` | 整段删除 |
| `metadata.managedFields` | 整段删除 |
| `spec.jobManager.podTemplate` / `spec.podTemplate` | 整段删除 |
| `initContainers[].env` | 只保留 `name`，`value` → `***` |
| `containers[].env` | 同上 |
| `status.reconciliationStatus.lastReconciledSpec` / `lastStableSpec` | 整段删除 |
| 日志行命中 `(?i)(password\|secret\|token\|ak\|sk\|access[_-]?key)` | 整行替换为 `***`（保留行号） |

日志返回上限 200 行；超出时只回尾部并标 `truncated: true`。

---

## 6. 限流与超时

| 项 | 默认 | 说明 |
|---|---|---|
| 每 token QPS | 5 | 令牌桶；超出 `rate_limited` 并带 `retryAfterMs` |
| 并发长任务 | 1 / token | 防并发 savepoint 打爆 HDFS |
| 批量上限 | 10 个作业 | 任何批量语义的工具 |
| 只读超时 | 10s | |
| 写操作超时 | 60s | savepoint 可能久 |
| 单次返回体 | ≤ 256KB | 超出截断并标 `truncated` |

---

## 7. 审计

每个工具调用写**一条**审计（复用 `audit` 表）：

```
action:  mcp.call
actor:   mcp:<token name>
target:  <namespace>/<name> 或 "*"
cluster: <cluster>
result:  ok | denied | error
detail:  {"tool":"flinkops_suspend","argsHash":"…","approvalId":"…","tokenId":"…","latencyMs":123,"truncated":false}
ip:      客户端 IP
```

只读工具也记（否则无法回答"AI 看过什么"）。

---

## 8. 错误模型（fail-closed）

MCP 的 tool 返回里 `isError: true` + 结构化正文；正文沿用平台错误结构，新增 3 个码：

| code | 何时 | AI 侧应如何理解 |
|---|---|---|
| `invalid_token` | token 不存在/失效/吊销 | 不要重试，让用户重建 token |
| `denied` | scope 不足或目标不在允许范围 | 不要换参数重试；直接告诉用户缺少什么权限 |
| `approval_required` | 需要审批（带 `approvalId`） | 提示用户去网页批准，然后带 `X-FlinkOps-Approval` 重试 |
| `approval_invalid` | token 已用/过期/参数不一致 | 重新发起审批 |
| `rate_limited` | QPS/并发超限 | 按 `retryAfterMs` 退避 |
| `insufficient_resources` | scale/提交超出集群剩余 | 报告差额，建议降并行度 |
| `unavailable` | 平台不可达 / JobManager REST 不通 | **不要**当成"没有数据" |

其余沿用 [`api-spec.md §1`](api-spec.md) 的码表。

---

## 9. 平台侧新增接口（本契约配套）

### POST /api/tokens

```json
{ "name": "claude-desktop", "scopes": ["read","operate"],
  "allowNamespaces": ["flink"], "allowJobs": ["flink/ant-*"],
  "qps": 5, "expiresInDays": 30 }
```
→ `201 { "id":"a1b2c3d4", "token":"fko_pat_a1b2c3d4_<secret>", "note":"只显示一次" }`

### GET /api/tokens
→ `{ items:[{id,name,scopes,allowNamespaces,allowJobs,qps,expiresAt,lastUsedAt,revokedAt}] }`（**不含 secret**）

### DELETE /api/tokens/{id}
→ `200 {"ok":true}`；写 `token.revoke` 审计

### GET /api/approvals?status=pending
→ `{ items:[{id,tool,target,argsHash,actor,tokenName,createdAt,status}] }`

### PATCH /api/approvals/{id}
```json
{ "decision": "approve" }   // 或 "reject"
```
→ `200 { "ok":true, "approvalToken":"…" }`（只在 approve 时返回一次）
→ 写 `approval.decide` 审计

> 审计新增 action：`mcp.call`、`token.create`、`token.revoke`、`approval.decide`。命名遵循 `资源.动作`。

---

## 10. 前端

- 设置页新增「MCP 与 Token」分块：token 列表（名称/scope/范围/最近使用/吊销）、创建向导（scope 勾选 + ns/job 通配 + QPS + 有效期）
- 新页面或告警中心旁新增「审批中心」：待批列表（tool/目标/参数摘要/发起者）+ 批准/拒绝；批准后一次性展示 approval token
- 作业详情页：若该作业有 pending 审批，显示横幅「AI 请求挂起此作业，待审批」

---

## 11. 版本与兼容

- `flinkops_mcp_version` 工具返回 `{ "api":"1", "tools":"1", "platform":"0.1.0" }`
- 破坏性变更（改参数名、删工具、收紧返回）→ `tools` +1
- MCP server 启动时校验 `/api/version` 在支持区间内，不匹配 → 所有工具返回 `unavailable` 并说明版本差

---

## 12. 测试矩阵（验收用例，全部必跑）

| # | 场景 | 期望 |
|---|---|---|
| 1 | 只读 token 调 `flinkops_suspend` | `denied`，message 指明缺 `operate`；审计 `result=denied` |
| 2 | `operate` token 挂起 `flink/order-sync`（范围外） | `denied` |
| 3 | `operate` token 挂起范围内作业、无审批 | `approval_required` + `approvalId` |
| 4 | 批准 A 作业后改参数去挂起 B | `approval_invalid`（argsHash 不一致） |
| 5 | 同一 approval token 用两次 | 第二次 `approval_invalid` |
| 6 | 删作业时 `confirmName` 不匹配 | `bad_request`，不执行 |
| 7 | 平台停机 | 所有工具 `unavailable`，**不是空列表** |
| 8 | `flinkops_prom_query` 传 `up{job="x"}` | `bad_request`（前缀不在白名单） |
| 9 | YAML 工具返回里 grep `password\|secret\|AK\|SK` | 0 命中，且 `dropped` 列出被删字段 |
| 10 | 连续 20 次调用（qps=5） | 触发 `rate_limited`，审计可查 |
| 11 | token 吊销后立即调用 | `invalid_token` |
| 12 | 挂起作业后详情（REST 不可达） | `flinkops_get_job` 仍返回 summary，checkpoint 区标降级 |

---

## 13. 落地清单（文件级）

```
crates/flinkops-mcp/
├─ Cargo.toml
├─ src/main.rs         双传输 + fail-closed + 日志走 stderr
├─ src/client.rs       平台 API 客户端（Bearer、超时、错误映射）
├─ src/auth.rs         token 解析/本地缓存/scope 判断（服务端仍是最终裁决）
├─ src/tools/read.rs   §3.1
├─ src/tools/operate.rs§3.2（审批前置）
├─ src/tools/admin.rs  §3.3（二次确认）
├─ src/redact.rs       §5
├─ src/errors.rs       §8
└─ tests/              §12 的 1–12

crates/flinkops-api/src/routes/
├─ tokens.rs           POST/GET/DELETE /api/tokens
├─ approvals.rs        GET/PATCH /api/approvals + GET /api/approvals/stream（SSE）
└─ mcp_guard.rs        scope/范围/限流/审批校验（供 tokens 与写接口共用）

crates/flinkops-store/src/lib.rs
└─ MIGRATIONS 常量      flinkops_mcp_tokens / flinkops_mcp_approvals（与其它表一样内联 DDL，
                        没有独立的 migrations/ 目录）

docs/
├─ mcp-spec.md（本文）
└─ api-spec.md（追加 §10 MCP 配套接口）
```

**实现顺序**：`tokens.rs` → `mcp_guard.rs` → `approvals.rs` → `flinkops-mcp` 只读工具 → operate + 审批 → admin → 前端两个面板 → 跑完 §12。

---

## 14. 实现状态（v1 已落地，2026-11-26）

| 契约条目 | 落地位置 | 状态 |
|---|---|---|
| §2.1 Token 形态 `fko_pat_<id>_<secret>`，库里只存 sha256 | `crates/flinkops-api/src/mcp_guard.rs`（`bearer_parts` / `sha256_hex`）、`routes/tokens.rs` | ✅ |
| §2.2 scope：read / operate / admin | `mcp_guard::scope_ok`；写接口按工具名取 operate，删除取 admin | ✅ |
| §2.3 资源范围 allow_ns / allow_jobs（空=无权，支持 `*` 通配） | `mcp_guard::job_allowed` + `check_job_scope`；创建时即拒绝"空范围" | ✅ |
| §2.4 QPS 令牌桶 | `AppState::mcp_rate` + `mcp_guard::take_rate`；超限返回 429（不再冒充 401） | ✅ |
| §4 一次性审批（300s 窗口、argsHash 绑定、单次核销） | `flinkops_mcp_approvals` + `mcp_guard::gate_write` + `routes/approvals.rs` | ✅ |
| §5 脱敏（YAML 段落删除 + env 打码 + 日志行打码） | `crates/flinkops-mcp/src/redact.rs`（含单测） | ✅ |
| §3 工具面 17 个（9 read / 7 operate / 1 admin） | `crates/flinkops-mcp/src/tools.rs` | ✅ |
| §6 审计 `token.create` / `token.revoke` / `approval.decide` | `routes/tokens.rs`、`routes/approvals.rs` | ✅ |
| §7 错误码 approval_required / approval_invalid / denied / unavailable | `gate_write` 返回 `Conflict`/`Forbidden`；MCP 侧原样透出给模型 | ✅ |
| §9 平台接口 `/api/tokens`、`/api/approvals` | `crates/flinkops-api/src/routes/mod.rs` | ✅ |
| 前端：Token 管理 + 审批中心 | `apps/web/src/views/Mcp.vue`（`/mcp`） | ✅ |
| 审批实时推送（SSE，不轮询） | `GET /api/approvals/stream` + `AppState::approvals_tx`（broadcast）；前端 EventSource + toast | ✅ |
| §11 HTTP transport（Streamable HTTP） | crates/flinkops-mcp/src/main.rs（--http host:port，POST /mcp 收单条或 batch，GET / 自述，GET /healthz） | ✅ |

### 实测（2026-11-26，8081 实例）

```
token 创建          → fko_pat_e6ab09c3_74678…
mcp --check         → ok: mcp:mcp-e2e @ http://127.0.0.1:8081
tools/list          → 17
list_jobs           → count=3 truncated=true
prom 白名单         → 400 bad_request（mysql_global_status_connections）
越界 models/kafka…  → 403 forbidden
scale 无审批        → 409 approval_required:ap_f99cfc6d5d23
审批中心批准        → approvalToken=apv_2cf5b9b408…
带审批重试          → 400 bad_request（parallelism 不能为 0，说明已过闸门、未改作业）
同 token 再用一次   → 403 approval_invalid（一次性生效）
吊销后 --check      → 401 unauthorized
```
