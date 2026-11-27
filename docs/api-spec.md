# API 规范（v1）

**简体中文** | [English](api-spec.en.md)

> 这份文档是**契约**。新增/修改接口先改这里，再改代码；实现须与本文档一致，文档未描述的行为视为未定义。

- Base path：同源，无前缀（`/api/...`）
- 认证：`HttpOnly` Cookie `flinkops_session`（登录后由服务端下发）
- 内容类型：请求/响应均为 `application/json; charset=utf-8`（日志与反代透明传输除外）
- 时间：RFC 3339 UTC，如 `2026-11-25T02:01:33Z`
- 字段命名：**一律 camelCase**（Rust 侧用 `#[serde(rename_all = "camelCase")]`）
- 数值：并发度、副本数、槽位等用整数；指标值用浮点

## 1. 统一错误

HTTP 状态码 + 固定结构：

```json
{ "error": { "code": "not_found", "message": "cluster prod 未接入" } }
```

| code | HTTP | 何时 |
|---|---|---|
| `unauthorized` | 401 | 未登录 / 会话失效 |
| `forbidden` | 403 | 只读模式、只读集群、反代写操作 |
| `not_found` | 404 | 集群/Crd/作业不存在 |
| `bad_request` | 400 | 参数非法、metrics 未传 ids；body 不是合法 JSON（同样返回 JSON 信封，不再是 text/plain） |
| `conflict` | 409 | 作业还没有 JobID、并发冲突 |
| `upstream_error` | 502 | kube-apiserver / JobManager REST 失败 |
| `internal` | 500 | 其它 |

**不要**新增其它顶层结构；**不要**把上游错误原文直接当 message 返回（要带上下文，见 `flinkops_types::Error::Upstream`）。

## 2. 认证与会话

### POST /api/login

```json
{ "username": "admin", "password": "admin", "totp": "123456" }
```
→ `200` + **两个** `Set-Cookie`：
`flinkops_session=<uuid>; HttpOnly; SameSite=Lax; Max-Age=86400`、
`flinkops_csrf=<token>; SameSite=Lax`（非 HttpOnly，供前端读出来放进 `X-CSRF-Token`）。
TLS 场景（或 `FLINKOPS_COOKIE_SECURE=true`）两个都带 `Secure`。

失败：
- 密码错 → `401 bad_credentials`；
- 开了 TOTP 但验证码错/缺 → `401 totp_invalid`；
- 同一 `IP+账号` 10 分钟内失败 5 次 → `429 too_many_attempts` + `Retry-After: 300`。

失败 → `401 {"error":{"code":"bad_credentials",...}}`，并写一条 `auth.login / denied` 审计。

> **message 里不再带 `bad request:` / `conflict:` 前缀**：机器判别看 `code`，message 只放人话（前端直接 toast）。

### CSRF（所有写操作）

带 Cookie 会话的 `POST/PUT/PATCH/DELETE`（`/api/login` 除外）必须回带 `X-CSRF-Token`，
值等于 `flinkops_csrf` Cookie；缺失/不一致 → `403 {"error":{"code":"csrf",...}}`。
Bearer Token（MCP）不受影响。

### POST /api/logout
清 Cookie → `200 {"ok":true}`

### POST /api/validate

`{ "yaml": "...", "cluster"?: "prod-a" }` → `{ findings, spec, hasErrors }`

校验规则与服务端提交时一致（同一套 `flinkops-core`）。其中 **`REST_NODEPORT` 只在
平台与被校验集群处于同一网络（`in_cluster`）时才提示**——平台在集群外时 NodePort
往往是唯一通路，此时提示"改 ClusterIP 走反代"不成立。传了 `cluster` 才会用到该判断，
未传按"未知"处理（保守提示）。

### POST /api/jobs/{cluster}/{ns}/{name}/image

`{ "image": "repo/app:tag", "upgradeMode": "savepoint|last-state|stateless" }`

- `upgradeMode` 默认 `savepoint`，只接受这三个值；
- **`upgradeMode=savepoint / last-state 需要 `state.savepoints.dir`**（作业 `flinkConfiguration` 或环境基线），
  未配置直接 `400` —— 否则 operator 触发 savepoint 失败，作业原地不动，用户看到的是"点了升级没反应"；
- 只接受 `FlinkDeployment`；`FlinkSessionJob` 的镜像属于它挂靠的会话集群，返回 `400` 并指出归属。

### POST /api/jobs/{cluster}/{ns}/{name}/autoscaler

`{ enabled, utilizationTarget, metricsWindow, stabilizationInterval, scaleUpMaxFactor, scaleDownMaxFactor, vertexMinParallelism, vertexMaxParallelism }`

写入 `spec.flinkConfiguration` 的 8 个官方键（常量表见 `routes/jobs/ops/autoscaler.rs`）。
operator **不认识**的输入（如 `excludedPeriods`）不会写入，响应里回 `ignored: [...]` 说明原因。
只支持 `FlinkDeployment`；写操作走 MCP 审批闸门（`flinkops_autoscaler`）。

### POST /api/jobs/{cluster}/{ns}/{name}/scale

`{ parallelism?, taskManagerReplicas? }`

- 槽位总量校验：`parallelism > taskManagerReplicas × taskmanager.numberOfTaskSlots` → `400`，
  并直接告诉你"需要把 TM 提到多少"；
- `FlinkSessionJob` 只允许改 `parallelism`（TM 副本属于会话集群，返回 `400` 指出归属）；
- 写操作走 MCP 审批闸门（`flinkops_scale`）。

### POST /api/jobs/{cluster}/{ns}/{name}/rest-exposed

`{ "kind": "ClusterIP|NodePort|LoadBalancer" }`

写 `kubernetes.rest-service.exposed.type`；operator 重建 Service 后生效（约 30–60s）。
只支持 `FlinkDeployment`。

### POST /api/me/password

`{ "oldPassword": "...", "newPassword": "..." }` → `200 {"ok":true}`。
新密码 ≥8 位且不能全数字；**viewer 也能改自己的密码**（自助服务不受"只读角色"限制）。
内置管理员（`FLINKOPS_ADMIN_*`）不支持，需改环境变量。

### POST /api/me/2fa/setup | enable | disable

- `setup` → `{secret, otpauth}`（还不生效，等确认）；
- `enable {code}` → 用验证器里的 6 位码确认，之后登录必须带 `totp`；
- `disable {code}` → 用当前动态码关闭。

### GET /api/me
```json
{ "user": "admin", "readonly": false, "clusters": ["prod"], "prometheus": false }
```
`readonly` 决定前端是否隐藏写按钮；`prometheus=false` 时大盘不显示历史。

### 鉴权覆盖范围（2026-11-27 修订）

- **只对 `/api/*` 与 `/proxy/*` 鉴权**。SPA 的 `index.html`、`/assets/*.js|css`、前端路由
  （`/login`、`/jobs`…）一律放行 —— 否则首次访问/会话过期时连登录页都打不开（实测返回一页 JSON 401）。
- **读接口同样受 namespace 白名单约束**（不只是写）：`/api/jobs|snapshots|session-jobs|sessions/{c}/{ns}/…`
  与 `/proxy/{c}/{ns}/…` 都会校验，越权返回 `403`。
- `/api/audit`、`/api/users`、`/api/tokens`、`/api/approvals`、`/api/settings`、`/api/admin`、`/api/clusters`
  仅 `admin` 可访问。
- 集合接口（`GET /api/jobs`、`/api/snapshots`）对非 admin 先过滤再计数，`total` 不会泄露未授权 ns 的作业总数。

## 3. 作业

### GET /api/jobs
Query：`cluster?`、`namespace?`、`refresh?`（`true` 时先从 K8s 同步一次）

```json
{
  "asOf": "2026-11-25T02:01:33Z",
  "indexAgeSecs": 3,
  // 资源规格直接来自 CRD；parallelismActual 只有「详情页算过」才有值（10 分钟缓存）
  // "jobManagerCpu": "0.5", "jobManagerMemory": "1024m",
  // "taskManagerCpu": "0.5", "taskManagerMemory": "2048m", "parallelismActual": 2,
  "items": [
    {
      "cluster": "prod", "namespace": "flink-jobs", "name": "orders",
      "kind": "FlinkDeployment", "state": "RUNNING", "jobId": "9f2c…",
      "parallelism": 8, "taskManagerReplicas": 4, "slotsPerTm": 2,
      "restartNonce": 3, "upgradeMode": "last-state",
      "image": "ghcr.io/…", "flinkVersion": "1.19",
      "restExposed": "ClusterIP", "jobManagerStatus": "READY",
      "reconciliationState": "DEPLOYED", "generation": 7, "observedGeneration": 7,
      "conditions": [{ "type": "Running", "status": "True", "reason": "JobRunning" }],
      "updatedAt": "2026-11-25T01:00:00Z",
      "health": "green"
    }
  ],
  "count": 1
}
```

- `health` 由后端算：`green`（`Running=True`）/ `yellow`（状态 RUNNING 但条件未确认，或调谐中）/ `red`（FAILED 或 `status.error` 非空）
- 列表数据来自**本地索引**（CRD 的物化视图），因此 `state` 最多滞后一个 reconcile 周期（默认 30s）

### POST /api/jobs/sync
无 body。→ `200 {"ok":true,"synced":29}`。逐集群拉取，单集群失败只告警不整体失败。

### GET /api/jobs/{cluster}/{ns}/{name}
返回 `JobDetail`：`summary` / `spec` / `checkpoint` / `savepoint` / `taskManager` / `clusterInfo`

```json
{
  "summary": { "...": "同列表行" },
  "spec": {
    "image": "ghcr.io/…", "flinkVersion": "1.19", "serviceAccount": "flink-sa",
    "restartNonce": 3, "parallelism": 8, "upgradeMode": "last-state",
    "jobState": "running", "jarURI": "s3://…", "entryClass": "com.example.Job",
    "args": ["--env","prod"], "taskManagerReplicas": 4,
    "jobManagerCpu": "1", "jobManagerMemory": "2Gi",
    "taskManagerCpu": "2", "taskManagerMemory": "4Gi",
    "flinkConfiguration": { "taskmanager.numberOfTaskSlots": "2" },
    "ingressTemplate": null,
    "raw": { "...": "spec 原样，供 CRD 编辑器" }
  },
  "checkpoint": { "last": { "id": 1284, "size": 13000000, "durationMs": 812 }, "completed": 1284, "failed": 3, "restored": 0, "inProgress": 0 },
  "savepoint": { "last": { "id": 41, "path": "s3://sp/41" }, "history": [] },
  "taskManager": { "replicas": 4, "labels": { "app": "orders" } },
  "clusterInfo": {}
}
```

`checkpoint.completed/failed` 来自 Flink REST；取不到时为 0（详情仍可用，不报错）。

### GET /api/jobs/{cluster}/{ns}/{name}/metrics
Query：`ids?`（逗号分隔，缺省取 `LIVE_METRICS` 那 10 个）

```json
{ "jobId": "9f2c…", "values": { "numRecordsInPerSecond": 32412 }, "source": "flink-rest", "sampledAt": "…", "degraded": null }
```

- 实时源失败时返回 `source:"none"|"prometheus"` + `degraded:"<原因>"`，`values` 为空 —— **前端必须显式提示，不能当成 0**
- 作业还没有 JobID → `409 conflict`
- `GET …/log?tail=N`：只返回最后 N 行（默认 2000，上限 20000）；不传 tail 也不再整份返回
- 禁止空 `ids`（会扫全量指标、拖慢 JM）

### GET /api/jobs/{cluster}/{ns}/{name}/log
Query：`kind=main|stdout` → `200 text/plain`

REST 文件日志不存在（K8s 默认只输出 stdout）→ `502 upstream_error`，message 会说明"请用 Pod logs"。

### POST /api/jobs/{cluster}/{ns}/{name}/state
```json
{ "running": false }
```
→ `200 {"ok":true,"state":"suspended"}`；平台只读 → `403`；写审计 `job.stop` / `job.start`。

### POST /api/jobs/{cluster}/{ns}/{name}/restart
无 body。改 `spec.restartNonce`（重建 JM，不动状态）→ `200 {"ok":true,"restartNonce":4}`。

### POST /api/jobs/{cluster}/{ns}/{name}/savepoint
```json
{ "path": "s3://flink-state/savepoints/orders" }
```
创建 `FlinkStateSnapshot`（而不是直接打 REST），→ `200 {"ok":true,"snapshot":"orders-sp-20261125020133"}`。

## 4. 平台

### GET /api/profiles
```json
{ "items": [ { "id": "general-1.19", "name": "general-1.19", "flinkVersion": "v1_19", "image": "…", "namespace": "flink-jobs", "checkpointPrefix": "s3://flink-state/checkpoints/{name}", "savepointPrefix": "…", "stateBackend": "rocksdb", "restExposed": "ClusterIP", "metricsPort": 9250, "serviceAccount": "flink-sa", "defaults": {…}, "labels": {…}, "builtin": true } ] }
```

### PUT /api/profiles/{id}
Body 为完整 `Profile`（可省 `id`，以路径为准；`builtin` 会被强制改为 false）。
→ `200 {"ok":true,"findings":[Finding…]}`，`findings` 是用该基线生成 spec 的校验结果，可直接渲染。

### GET /api/audit

Query：`action/actor/result/namespace/q/since/limit(≤5000)/offset`，`format=csv` 直接下载
（带 BOM，Excel 可识别）；响应 `{items,count,total,limit,offset}`。仅 admin。

### GET /api/audit (legacy)
→ `{"items":[{ "id","ts","actor","action","target","cluster","namespace","result","detail" }]}`，按时间倒序，上限 200 条。

审计 `action` 命名（新增时必须沿用）：
`auth.login` / `jobs.sync` / `job.start` / `job.stop` / `job.restart` / `savepoint.trigger` / `profile.save` / `ui.open` / `proxy.blocked`

`result`：`ok` | `denied` | `error`

### GET /api/sessions
Query：`cluster?`
```json
{ "items": [ { "cluster","namespace","name","state","flinkVersion","image","jobManagerCpu","jobManagerMemory","taskManagerReplicas","slotsPerTm","usedSlots","idleTtl","sqlGateway" } ] }
```
只返回 `spec.mode=session` 的 `FlinkDeployment`。

## 5. Flink UI 只读反代

```
ANY /proxy/{cluster}/{ns}/{name}/{*path}
```

| 规则 | 行为 |
|---|---|
| `GET/HEAD/OPTIONS` | 透传到 `http://{name}-rest.{ns}.svc.cluster.local:8081/{path}`，改写 `Location` |
| `PATCH ?mode=cancel`、`*/stop`、`*/savepoints`、`*/jars*`、`*/rescaling`、`DELETE *` | `403 {"code":"readonly_proxy"}`，写 `proxy.blocked` 审计 |
| `Upgrade: websocket` | `501 {"code":"ws_not_supported"}`（UI 降级轮询） |
| 未登录 | `401` |

**判定必须同时看 path 与 query**（`?mode=cancel` 在 query 里，只看 path 会漏）。

## 5.1 提交作业（新增）

### POST /api/jobs

Body 两种写法（任选其一）：

```json
{ "yaml": "apiVersion: flink.apache.org/v1beta1\nkind: FlinkDeployment\n…" }
```
```json
{ "object": { "apiVersion": "…", "kind": "FlinkDeployment", "metadata": {…}, "spec": {…} } }
```

Query：`dryRun=true` 时**不落库**，但会调用 apiserver 的 `dryRun=All` 走一遍 CRD schema + webhook
（2026-11-27 起）：平台自己的 `core::validate` 过不代表 webhook 会接受，预检要拿真裁决。
预检不消耗 MCP 一次性审批，审计记 `job.validate`（ok/error），与真实提交 `job.submit` 区分。

处理顺序（顺序不能变）：
1. 解析 YAML/JSON → 必须是 `FlinkDeployment`（`FlinkSessionJob` 请用 `/api/sessions` 系列；
   但 `PUT /api/jobs/{c}/{ns}/{n}` 两者都支持，按 body 的 `kind` 定位对象）
2. `core::validate(spec)`，有 `error` 级 finding 直接 `400`，响应体带 `findings`
3. 检查同名对象是否已存在 → `409 conflict`
4. `k8s.apply`（或 dryRun 跳过）
5. 写审计 `job.submit`，detail 记录 `image` 与 `parallelism`

```json
{ "ok": true, "name": "orders-etl", "namespace": "flink-jobs", "dryRun": false }
```

错误：

| 场景 | HTTP | code |
|---|---|---|
| YAML 解析失败 | 400 | `bad_request` |
| kind 不是 FlinkDeployment | 400 | `bad_request` |
| 校验有 error | 400 | `bad_request`（响应含 `findings`） |
| 已存在同名 | 409 | `conflict` |
| 平台只读 | 403 | `forbidden` |

### POST /api/jobs/{cluster}/{ns}/{name}/state 的兄弟接口

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/jobs/{c}/{ns}/{name}/restart` | bump `spec.restartNonce` |
| POST | `/api/jobs/{c}/{ns}/{name}/savepoint` | 创建 `FlinkStateSnapshot` |

## 5.2 事件与异常（新增）

### GET /api/jobs/{cluster}/{ns}/{name}/events

读 `core/v1 Event`，按 `involvedObject.name` + `kind=FlinkDeployment` 过滤（fieldSelector 在服务端完成，避免拉全量）。

```json
{ "items": [ { "type": "Warning", "reason": "BackOff", "message": "…", "count": 7,
               "firstTimestamp": "2026-11-25T01:00:00Z", "lastTimestamp": "2026-11-25T02:00:00Z",
               "source": "kubelet" } ], "count": 1 }
```

### GET /api/jobs/{cluster}/{ns}/{name}/exceptions

透传 Flink REST `/jobs/:id/exceptions`：

```json
{ "rootException": "java.lang.OutOfMemoryError: Java heap space", "truncated": false, "exceptions": [] }
```

作业没有 JobID → `409`；REST 不可达 → `502`（message 会说明"用 Flink UI 或 Pod 日志"）。

## 5.3 快照列表（新增）

### GET /api/snapshots

Query：`cluster?`、`namespace?`、`state?`（`COMPLETED|IN_PROGRESS|FAILED`）

list `flinkstatesnapshots`，按创建时间倒序：

```json
{ "items": [ { "cluster": "prod", "namespace": "flink-jobs", "name": "orders-sp-20261125020133",
               "jobName": "orders", "state": "COMPLETED", "path": "s3://flink-state/savepoints/orders/…",
               "createdAt": "2026-11-25T02:01:33Z" } ], "count": 1 }
```

`state` 未知时返回 `UNKNOWN`（不猜测）。
## 5.4 创建会话集群（新增）

### POST /api/sessions

```json
{ "name": "flink-sql-session", "cluster": "prod", "namespace": "flink-jobs",
  "profileId": "general-1.19", "slots": 4, "taskManagerReplicas": 2, "dryRun": false }
```

行为：用「环境基线」+ 覆盖项渲染 spec → 设 `spec.mode=session` → 去掉 `job` 段 → 校验 → 重名检查 → apply。
审计 `session.create`。

```json
{ "ok": true, "cluster": "prod", "namespace": "flink-jobs", "name": "flink-sql-session", "dryRun": false }
```

session 模式**不要求** `job.jarURI` / `job.parallelism`（`core::validate` 已按 `spec.mode` 放宽，有单测保护）。

## 5.5 恢复 / 取消 / 回滚（2026-11-27 修订）

三者的语义差异必须写死，前端文案也要跟着改：

| 动作 | 端点 | 对 CR 的改动 | 是否保留状态 | 是否重启 |
|---|---|---|---|---|
| 挂起 | `POST …/state {running:false}` | `spec.job.state=suspended` | 保留（savepoint 模式下先打快照） | 停 |
| 取消 | `POST …/cancel` | `state=suspended` + `upgradeMode=stateless` | **不保留**（不产生新快照） | 停 |
| 从 savepoint 恢复 | `POST …/restore {path?}` | 挂起 → `initialSavepointPath=path` → `state=running` | 恢复指定快照 | 重建 |
| 回滚镜像 | `POST …/rollback {image?}` | `spec.image=image` + `upgradeMode=savepoint` | 保留（走 savepoint 升级） | 重建 |

`restore` 的关键点（都是踩过的坑）：

- **路径两种写法都接受**：Flink/operator 记录的目录是 `…/savepoint-abc-123`（无尾斜杠），
  平台内部补 `/`，不再 400；
- **必须两步**：对已 reconcile 的作业只写 `initialSavepointPath` 不会触发任何部署，
  必须先 `suspended`、等到 `lifecycleState=SUSPENDED`、再 `running`；
- 返回体带 `confirmed`：等到新 `jobId` 且 `RUNNING` 才为 `true`；
  超时返回 `false` + `note`（"已提交但未确认"），前端不能把它当成功；
- 超时预算：挂起 90s、启动 180s，前端调用用 `timeoutMs=240000`。

## 6. 新增接口五步

1. **写文档**：在本文档补一节（路径、方法、请求、响应、错误、权限、审计 action）。
2. **定 DTO**：放 `flinkops-types`（`camelCase` + `cargo test` 覆盖提取/序列化）。
3. **写 handler**：在 `crates/flinkops-api/src/routes/<资源>.rs`；
   - 签名固定：`State<Arc<AppState>>` 先，`HeaderMap` 次，`Path`/`Query`/`Json` 后；
   - 第一行：`let user = st.require_user(&headers).await?;`（除公开接口）；
   - 写操作：先查 `st.cfg.readonly`，再执行，再 `st.audit(...)`。
4. **注册路由**：`routes/mod.rs` 里加 `.route(...)`（axum 0.8 用 `{param}` 语法）。
5. **补测试**：`flinkops-core` 加纯逻辑单测；`routes/proxy.rs` 已有 `is_write_path` 单测可参照。

模板：

```rust
// crates/flinkops-api/src/routes/jobs.rs
#[derive(Deserialize)]
pub struct FooQuery { pub limit: Option<i64> }

pub async fn foo(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((cluster, ns, name)): Path<(String, String, String)>,
    Query(q): Query<FooQuery>,
) -> ApiResult<Json<Value>> {
    let _ = st.require_user(&headers).await?;
    let c = st.cluster(&cluster)?;
    let obj = c.get_raw(flinkops_types::crd::KIND_DEPLOYMENT, &ns, &name).await?;
    Ok(Json(json!({ "name": name, "limit": q.limit })))
}
```

## 7. 还没实现（roadmap）

| 能力 | 备注 |
|---|---|
| `POST /api/jobs`（提交） | 需要 `core::spec_from_profile` + `k8s.apply`，逻辑已具备 |
| `POST /api/jobs/{…}/validate`、`/diff` | `core::validate` / `core::diff` 已实现且有单测，只差暴露 |
| `GET /api/prometheus/query` | 透传 PromQL（`metrics.backend=prometheus` 时用） |
| `GET/POST /api/snapshots` | `FlinkStateSnapshot` 列表与创建（`k8s.create_savepoint` 已有） |
| `POST /api/sessions` | 创建 session 集群（需 `core` 生成 spec） |
| OIDC 登录 | 当前仅本地账号 + 内存会话 |
| WebSocket 反代 | 需 `hyper::upgrade` + `tokio-tungstenite` |

## 8. 接口总表（与实现同步）

> 这一节是"速查表"。详细字段见上文各节；新增接口必须在这里登记。

### 8.1 系统与认证

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/healthz` | 存活（免登录） |
| GET | `/readyz` | 就绪：DB 可读 + 至少一个集群（免登录） |
| GET | `/api/version` | 版本 / 集群数 / 指标源（免登录） |
| POST | `/api/login` | `{username,password}` → HttpOnly cookie |
| POST | `/api/logout` | 清 cookie |
| GET | `/api/me` | 当前用户 / 集群列表 / readonly / prometheus |

### 8.2 作业

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/jobs` | 列表；`?cluster=&namespace=&refresh=true` |
| POST | `/api/jobs` | 提交 FlinkDeployment；`?cluster=&dryRun=true` |
| POST | `/api/jobs/sync` | 全量同步（含孤儿索引清理） |
| GET | `/api/jobs/{c}/{ns}/{n}` | 详情 |
| DELETE | `/api/jobs/{c}/{ns}/{n}` | 删除作业 |
| GET | `…/metrics` | REST 实时指标（`?ids=`） |
| GET | `…/log` | JM 日志（`?kind=main\|stdout`） |
| GET | `…/yaml` | CRD 原文 YAML |
| GET | `…/events` | K8s 事件（fieldSelector 过滤） |
| GET | `…/exceptions` | Flink 根因异常 |
| GET | `…/backpressure` | 顶点背压（`?vertex=`，30–60s 一次） |
| GET | `…/flamegraph` | 顶点火焰图 Traces JSON（`?type=FULL\|ON_CPU\|OFF_CPU`） |
| POST | `…/profile` | async-profiler（Flink 1.18+） |
| POST | `…/state` | `{running:bool}` 启停 |
| POST | `…/restart` | bump `restartNonce` |
| POST | `…/savepoint` | 创建 FlinkStateSnapshot（`{path?}`） |
| POST | `…/restore` | 从 savepoint 恢复（`{path?}`）；**挂起→写路径→启动**，返回 `confirmed` |
| POST | `…/cancel` | 取消（不保留状态）：`upgradeMode=stateless` + `state=suspended` |
| POST | `…/rollback` | 回滚镜像（`{image?}`，留空取审计里的上一个镜像） |
| POST | `…/scale` | `{parallelism?, taskManagerReplicas?}` |
| POST | `…/image` | `{image, upgradeMode}` 滚动升级 |
| POST | `…/ingress` | `{host, className?}` 创建域名直连 |
| POST | `…/autoscaler` | `{enabled}` 开关 autoscaler |

### 8.3 会话集群 / 快照 / 模板

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/sessions` | session 集群列表（判定：`spec` 无 `job` 段） |
| POST | `/api/sessions` | 创建 `mode=native` + 无 `job` 的 FlinkDeployment |
| GET | `/api/snapshots` | FlinkStateSnapshot 列表；CRD 缺失时返回 `unsupportedClusters` |
| DELETE | `/api/snapshots/{c}/{ns}/{n}` | 删除快照 |
| GET | `/api/templates` | 模板列表 |
| POST | `/api/templates` | `{name, fromJob?}` 或 `{name, payload}`（**自动脱敏**） |
| DELETE | `/api/templates/{id}` | 删除模板 |

### 8.4 平台治理

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/profiles` / PUT `/api/profiles/{id}` | 环境基线 |
| GET | `/api/audit` | 审计（最近 200 条） |
| GET | `/api/cluster-info` | operator 镜像 + CRD 能力 + status 陈旧计数 |
| POST | `/api/clusters` | 动态接入集群（`kubeconfigPath` 或 `kubeconfig` 粘贴内容；持久化到库） |
| POST | `/api/clusters/test` | 接入前测试连通性（只读探活，不落库） |
| DELETE | `/api/clusters/{id}` | 移除动态集群（env 里的拒绝） |
| POST | `/api/clusters/{id}/reconnect` | 按原接入方式重建 client |
| GET | `/api/clusters/{id}/credential` | 接入信息（**脱敏，不含私钥**） |
| GET | `/api/settings` / POST `/api/settings` | 运行期配置（保留期 / DB 上限 / Prometheus） |
| POST | `/api/settings/reset` | 恢复默认 |
| GET | `/api/admin/diagnostic` | 诊断包（可安全分享） |
| GET | `/api/admin/backup` | 备份信息 |
| POST | `/api/admin/vacuum` | SQLite VACUUM |

### 8.5 告警

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/alerts/rules` | 规则列表 + 可选 kind |
| POST | `/api/alerts/rules` | 新建/更新规则 |
| DELETE | `/api/alerts/rules/{id}` | 删除规则 |
| GET | `/api/alerts/events` | 事件（`?state=OPEN\|ACK`） |
| POST | `/api/alerts/events/{id}/ack` | 确认事件 |

规则 kind：`job_not_running` / `restarts_high` / `nodeport_exposed` / `slots_over`，后台每 60s 求值，同规则+作业只保留一条 OPEN。

### 8.6 监控与反代

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/metrics/query` | PromQL 瞬时查询 |
| GET | `/api/metrics/query_range` | 区间查询（历史曲线） |
| GET | `/api/prometheus/discover` | 自动发现候选 Service |
| POST | `/api/observability/podmonitor` | apply PodMonitor（`{yaml}`） |
| POST | `/api/observability/patch-labels` | 给作业补 metadata.labels |
| ANY | `/proxy/{c}/{ns}/{n}` 与 `…/{*path}` | Flink UI 只读反代（含 `/ws`） |

## 8.5 写操作的并发与幂等（2026-11-27 新增，必须遵守）

三条规则，新增写接口时**不要绕过**：

1. **同一动作 15s 单飞**：中间件对 `/api/jobs/{c}/{ns}/{n}/{action}` 的写请求做
   `METHOD path` 级占位，重复请求直接 `409 {"code":"conflict","message":"这个操作刚提交过…"}`。
   起因：3 个并发 `POST …/restart` 全部 200、`restartNonce` 连加 3，operator 连做 3 轮滚动升级，
   最后一轮引用了已被 retention 清掉的 checkpoint，作业直接 `FAILED`。
2. **生命周期守卫**：`status.lifecycleState ∈ {UPGRADING, ROLLING_BACK}` 时，
   `restart|state|image|scale|restore` 一律 `409`（提示等状态稳定），不再"再来一次"。
3. **读-改-写必须带 `metadata.resourceVersion`**（`PUT /api/jobs/{c}/{ns}/{name}` 支持
   `If-Match: <resourceVersion>`，也认 body 里的 `metadata.resourceVersion`；
   `/yaml` 已保留该字段，编辑器的"保存"因此是真 CAS，冲突返回 409）：merge patch 本身没有 CAS，
   统一走 `Cluster::patch_merge_cas()`（首次读 RV → 写进 patch 体 → apiserver 判 409）。
   `restartNonce` 的冲突**不再自动重试**（重试等于"用户点一次、生效多次"），
   冲突直接返回 `409`，由调用方决定是否重试。

前端配合：动作按钮在请求期间 `disabled`，处理函数开头 `if (busy) return`，
并在 toast 里原样展示 409 的 message（它已经说明了"刚才提交过/正在升级"）。

## 9. 请求体字段规范（踩过的坑）

**Flink autoscaler 的键名**（写错不会报错，只会静默无效）：

| 正确键 | 说明 |
|---|---|
| `job.autoscaler.enabled` | 总开关（只采集指标 + 建议） |
| `job.autoscaler.scaling.enabled` | **真正执行 vertex 伸缩**，缺了它等于没开 |
| `job.autoscaler.target.utilization` | 目标利用率（旧代码写成 `job.autoscaler.utilization.target`，无效） |
| `job.autoscaler.target.utilization.boundary` | 容忍区间 |
| `job.autoscaler.metrics.window` / `stabilization.interval` | 窗口 / 稳定期 |
| `job.autoscaler.scale-up.max-factor` / `scale-down.max-factor` | 单次伸缩倍数 |

`job.autoscaler.excluded.periods` **不是** Flink 的配置键，平台不再落盘。

**所有请求体 struct 必须加 `#[serde(rename_all = "camelCase")]`。**

漏了会导致：前端传 `upgradeMode` / `taskManagerReplicas` / `auditRetentionDays`，
后端字段是 `upgrade_mode` 等 → 反序列化成 `None` → 走默认值 → **接口 200 但值没生效**。
这个坑在本项目里踩过 3 次（ImageBody、ScaleBody、SaveBody），已列为 review 必查项。

## 10. MCP 配套接口（见 [`mcp-spec.md`](mcp-spec.md)）

MCP 接入所需的机器认证与审批接口，契约细节在 `mcp-spec.md §9`，此处只登记路径以便总表完整：

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/tokens` | 创建 API Token（返回 secret 仅一次） |
| GET | `/api/tokens` | Token 列表（不含 secret） |
| DELETE | `/api/tokens/{id}` | 吊销 Token |
| GET | `/api/approvals` | 待审批列表（`?status=pending`） |
| PATCH | `/api/approvals/{id}` | 批准/拒绝，批返回一次性 approval token |

新增审计 action：`mcp.call`、`token.create`、`token.revoke`、`approval.decide`（沿用 `资源.动作` 命名）。
MCP 侧错误码在 `mcp-spec.md §8`（`invalid_token` / `denied` / `approval_required` / `approval_invalid` / `rate_limited` / `insufficient_resources` / `unavailable`）。

## 10. 端点总表（与路由表同步，CI 有契约测试校验）

> 本表由 `crates/flinkops-api/src/routes/mod.rs` 生成。新增/删除端点时同步更新；
> `crates/flinkops-api/tests/api_contract.rs` 会检查「这里写了的 GET 端点路由表里必须存在」。

| 方法 | 路径 |
|---|---|
| ANY | `/api/{*rest}` |
| GET | `/api/admin/backup` |
| GET | `/api/admin/crd-schema/{name}` |
| GET | `/api/admin/diagnostic` |
| POST | `/api/admin/vacuum` |
| GET | `/api/alerts/events` |
| POST | `/api/alerts/events/{id}/ack` |
| GET | `/api/approvals` |
| PATCH | `/api/approvals/{id}` |
| GET | `/api/approvals/stream` |
| GET | `/api/audit` |
| GET | `/api/catalogs/{id}/diff` |
| POST | `/api/catalogs/{id}/sync` |
| GET | `/api/catalogs/{id}/tables` |
| GET | `/api/catalogs/complete` |
| GET | `/api/cluster-info` |
| POST | `/api/clusters` |
| POST | `/api/clusters/test` |
| GET | `/api/clusters/{id}/credential` |
| POST | `/api/clusters/{id}/reconnect` |
| POST | `/api/diff` |
| POST | `/api/export/xlsx` |
| POST | `/api/format` |
| GET | `/api/jobs` |
| GET | `/api/jobs/{cluster}/{ns}/{name}/events` |
| GET | `/api/jobs/{cluster}/{ns}/{name}/log` |
| POST | `/api/jobs/{cluster}/{ns}/{name}/scale` |
| POST | `/api/jobs/sync` |
| POST | `/api/login` |
| POST | `/api/logout` |
| GET | `/api/me` |
| POST | `/api/me/2fa/disable` |
| POST | `/api/me/2fa/enable` |
| POST | `/api/me/2fa/setup` |
| POST | `/api/me/password` |
| GET | `/api/metrics/query` |
| GET | `/api/metrics/query_range` |
| GET | `/api/profiles` |
| PUT | `/api/profiles/{id}` |
| GET | `/api/prometheus/discover` |
| POST | `/api/render` |
| GET | `/api/sessions` |
| GET | `/api/settings` |
| POST | `/api/settings/reset` |
| GET | `/api/snapshots` |
| POST | `/api/sql/execute` |
| GET | `/api/sql/jobs` |
| POST | `/api/sql/jobs/{id}/deploy` |
| POST | `/api/sql/jobs/{id}/deploy-as-app` |
| POST | `/api/sql/jobs/{id}/resume` |
| POST | `/api/sql/jobs/{id}/suspend` |
| GET | `/api/sql/jobs/{id}/versions` |
| GET | `/api/tokens` |
| DELETE | `/api/tokens/{id}` |
| GET | `/api/users` |
| DELETE | `/api/users/{username}` |
| POST | `/api/validate` |
| GET | `/api/version` |
| GET | `/healthz` |
| ANY | `/proxy/{cluster}/{ns}/{name}` |
| ANY | `/proxy/{cluster}/{ns}/{name}/` |
| ANY | `/proxy/{cluster}/{ns}/{name}/{*path}` |
| GET | `/readyz` |

`ANY` 表示同一路径的多个方法都挂到了同一个 handler（多为只读反代）。
