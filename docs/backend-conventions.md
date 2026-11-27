# 后端编码规范

[English](backend-conventions.en.md) | **简体中文**

> 分层：`types → core → 适配器（k8s / flink-rest / store）→ api`，每层只有一个职责；新增代码按职责落位，不跨层调用。

## 1. 分层与依赖方向

```
flinkops-api  ──►  flinkops-k8s       (kube 动态客户端)
     │        ──►  flinkops-flink-rest (JM REST)
     │        ──►  flinkops-store      (sqlx::Any)
     │        ──►  flinkops-core       (纯逻辑：合并/校验/diff/健康度)
     └──────────►  flinkops-types      (DTO + CRD 提取 + 错误)
```

**硬规则**

| 层 | 可以做 | 不可以做 |
|---|---|---|
| `types` | serde 结构、CRD 字段提取、错误枚举 | 依赖 kube / axum / sqlx |
| `core` | 纯函数：基线合并、校验、diff、健康度、指标源选择 | 依赖 axum / kube / 网络 |
| `k8s` | 读写 `flink.apache.org/v1beta1`（DynamicObject） | 组装 HTTP 响应、写 DB |
| `flink-rest` | 调 JobManager REST | 缓存、业务判断 |
| `store` | SQL 与迁移；SQL 必须经 `self.sql()` 走方言改写 | 业务规则（比如"什么时候算失败"） |
| `api` | 路由、鉴权、审计、编排 | **直接写复杂业务逻辑**（应下沉到 core） |

违反依赖方向 = review 直接打回。

## 2. api 内部的模块划分

```
crates/flinkops-api/src/
├─ main.rs        启动装配：tracing → Config → Store → Clusters → AppState → router → serve
├─ lib.rs         模块声明 + 再导出（bin 与 lib 同名 crate，bin 通过 flinkops_api::* 引用）
├─ config.rs      环境变量 → Config（附 parse 单测）
├─ state.rs       AppState（store/clusters/sessions/http）+ require_user + audit
├─ error.rs       ApiError → HTTP（唯一错误出口）
└─ routes/
   ├─ mod.rs      路由表（新增接口只改这里 + 对应模块）
   ├─ system.rs   healthz / readyz / version（免登录）
   ├─ auth.rs     login / logout / me
   ├─ jobs.rs     列表/详情/指标/日志/启停/重启/savepoint + sync_clusters
   ├─ profiles.rs 基线读写
   ├─ audit.rs    审计列表
   ├─ sessions.rs 会话集群列表
   └─ proxy.rs    Flink UI 只读反代（含 is_write_path 单测）
```

新增一类资源 → 新建 `routes/<资源>.rs`，**不要**往 jobs.rs 里堆。

## 3. 命名

| 对象 | 规则 | 示例 |
|---|---|---|
| crate | `flinkops-<层>` | `flinkops-flink-rest` |
| 模块/文件 | 小写下划线，单数 | `routes/sessions.rs` |
| 处理函数 | 动词短语，与资源语义一致 | `set_state` / `bump_restart_nonce`（在 k8s）、`savepoint` |
| DTO | 名词，`View`/`Summary`/`Detail` 后缀表示视角 | `JobSummary` / `JobDetail` / `JobSpecView` |
| 错误码 | 小写下划线，稳定不改 | `readonly_proxy` / `upstream_error` |
| 审计 action | `资源.动作`，全小写 | `job.restart` / `savepoint.trigger` / `proxy.blocked` |
| 环境变量 | `FLINKOPS_` 前缀 | `FLINKOPS_CLUSTERS` |

## 4. 错误处理

- 领域错误只用 `flinkops_types::Error`（`NotFound/BadRequest/Unauthorized/Forbidden/Conflict/Upstream/Internal`）。
- HTTP 层只在 `error.rs` 做一次映射；handler 返回 `ApiResult<T>`。
- **禁止** `unwrap()` / `expect()`（`main` 与测试除外）；禁止 `let _ = result;` 吞错——要么 `?`，要么 `warn!` 记录。
- 上游错误必须带上下文：
  ```rust
  Error::Upstream { target: format!("{name} 日志"), message: e.to_string() }
  ```

## 5. handler 骨架（复制即用）

```rust
pub async fn handler(
    State(st): State<Arc<AppState>>,   // 1. 状态
    headers: HeaderMap,                // 2. 头部（鉴权/审计需要）
    Path((cluster, ns, name)): Path<(String, String, String)>,
    Query(q): Query<MyQuery>,          // 或 Json(body): Json<MyBody>
) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;      // 3. 鉴权（公开接口省略）
    if st.cfg.readonly { return Err(ApiError(Error::Forbidden("平台处于只读模式".into()))); }
    let c = st.cluster(&cluster)?;                     // 4. 取集群句柄
    let obj = c.get_raw(flinkops_types::crd::KIND_DEPLOYMENT, &ns, &name).await?;
    // 5. 业务（复杂逻辑放 core）
    st.audit(&user, "job.xxx", &name, &cluster, &ns, "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}
```

顺序固定：`State` → `HeaderMap` → 其它提取器。这是 axum 能识别 Handler 的前提。

## 6. 数据源规则（这是本项目最容易写错的地方）

| 数据 | 来源 | 理由 |
|---|---|---|
| 作业列表 / 状态 / 条件 | **CRD**（本地索引） | 一次 list 拿全部；实时性由 reconcile 周期决定（≤30s） |
| 实时指标 / 背压 / 水位线 | **Flink REST**（`?get=` 批量） | 毫秒级；Prometheus 有 15–60s 延迟 |
| 历史 / 跨作业聚合 | Prometheus（可选） | 只有它能给历史 |
| checkpoint 计数 | REST `/jobs/:id/checkpoints` | CRD 里只有 `lastCheckpoint` |
| 日志 | **Pod logs 优先**；REST 文件日志需 log4j 配 appender | K8s 默认只有 stdout |
| 谁在跑（真相） | `status.conditions[Running=True]` | 比 Pod Ready 可信 |

**不要**：为了"实时"去高频轮询 `/backpressure`（每次采样 100 次任务线程，30–60s 一次即可）。

## 7. 写操作的三条纪律

1. **幂等优先**：重启用 bump `restartNonce`，不要删 Pod。
2. **留痕**：任何改变集群状态的操作，先 `audit(... "ok")` 或 `"denied"`，再返回。
3. **只读保护**：`cfg.readonly` 与 `Cluster.readonly` 双重检查（平台级 + 集群级）。

## 8. 测试要求

| 层 | 要求 |
|---|---|
| `types` | CRD 提取必须有 fixture 单测（覆盖 status 缺失、session job、字符串数字） |
| `core` | 每个公共函数至少 1 个单测；合并要证明**幂等** |
| `k8s` / `flink-rest` | 不写单测，靠 e2e（`kind` + operator chart） |
| `api` | 纯函数（如 `is_write_path`）必须有单测；handler 靠冒烟脚本 |

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 9. 前端协作约定

- 后端只输出 camelCase；前端不写转换层。
- 所有列表返回 `{ items, count }`（或 `{ items }`），不要裸数组——便于以后加分页字段。
- 指标类接口必须带 `source` 与 `degraded`，前端据此显示"实时源不可用"。
- 前端类型由 `ts-rs` 从 Rust DTO 生成（见 `apps/web/README.md`），禁止手写第二份。

## 10. 提交前自检

1. `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
2. 新接口已写入 `docs/api-spec.md`
3. 新增依赖在 `deny.toml` 白名单内（`cargo deny check licenses`）
4. 没有把 kubeconfig / 密码 / token 写进仓库（`.secrets/` 已忽略）

## 7. 结构现状（2026-11-27 重构后）

```
crates/flinkops-api/src/
├─ main.rs        启动装配（只做接线）
├─ lib.rs         模块声明
├─ config.rs      环境变量 → Config
├─ state.rs       AppState + require_user/principal + audit
├─ error.rs       ApiError → HTTP（唯一错误出口）
├─ extract.rs     AuthUser / RequireWrite / JobTarget（鉴权与只读检查的声明式入口）
└─ routes/
   ├─ mod.rs      路由表 + 中间件（rbac / csrf / action_guard / audit）
   ├─ jobs/       作业域（按读写分组，mod.rs 只放共享 helper）
   │  ├─ list.rs       列表 / 详情 / 指标 / 日志 / 事件 / 顶点诊断
   │  ├─ lifecycle.rs  状态 / 重启 / savepoint / 取消 / 回滚 / 恢复 / 删除
   │  ├─ ops.rs        镜像 / 并行度 / Ingress / REST 暴露 / autoscaler / 蓝绿
   │  └─ submit.rs     dry-run / 创建 / PUT 更新
   ├─ sql.rs       SQL 工作台（下一个拆分目标，1247 行）
   └─ ...          其它按资源一文件
```

`crates/flinkops-store/src/`：`lib.rs` 只留 `connect/pool/migrate` 与迁移定义，
其余按领域拆到 `jobs / audit / sessions / mcp / profiles / alerts / catalogs / platform / users`。

### handler 写法（新规范，强约束）

```rust
pub async fn restart(
    State(st): State<Arc<AppState>>,   // 状态
    u: AuthUser,                       // 已登录（缺了编译不过）
    w: RequireWrite,                   // 平台可写（写端点必须有）
    j: JobTarget,                      // /{cluster}/{ns}/{name}
) -> ApiResult<Json<Value>> {
    let AuthUser { user, headers, .. } = u;
    let _w = w;
    let JobTarget { cluster, ns, name } = j;
    // 业务：不再手写 require_user / readonly 判断
}
```

**不要再写** `let user = st.require_user(&headers).await?;` 和
`if st.cfg.readonly { ... }` —— 这两行是历史上漏鉴权、只读平台仍可写的主要来源。

### 结构总览（2026-11-27 第三轮重构后）

```
crates/flinkops-api/src/
├─ main.rs        启动装配（只做接线）
├─ lib.rs         模块声明
├─ config.rs      环境变量 → Config
├─ state.rs       AppState + require_user/principal + audit
├─ error.rs       ApiError → HTTP（唯一错误出口）
├─ extract.rs     AuthUser / RequireWrite / JobTarget（声明式鉴权）
├─ totp.rs        TOTP（两步验证）
└─ routes/
   ├─ mod.rs                   只留路由表 + 中间件装配（319 行）
   ├─ middleware/              CSRF / RBAC / 写操作单飞 / 失败审计 / 错误信封
   ├─ jobs/{list,lifecycle,ops,submit}.rs    作业域按读写分组
   ├─ sql/{execute,jobs,deploy,catalog}.rs   SQL 工作台按职责分组
   └─ ...                     其它资源一文件
```

**判定标准**：单个文件超过 ~600 行就该问一句"它是不是两个模块"。
`routes/mod.rs`（664→319）、`jobs.rs`（2409→5 个 ≤640）、`sql.rs`（1247→4 个 ≤541）、
`store/lib.rs`（1969→495 + 8 个领域文件）都是按这条线拆的。

### 迁移脚本（一次性，别复制进代码）

拆分是"整项搬移"：用脚本按顶层项切分、保留 attributes/doc 注释，
再让 `cargo fmt` + `clippy -D warnings` + 42 个测试兜底。逐文件搬，不混逻辑改动。
