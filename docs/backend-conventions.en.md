# Backend conventions

**English** | [简体中文](backend-conventions.md)

> Layering: `types → core → adapters (k8s / flink-rest / store) → api`, one responsibility per layer.
> New code goes to the layer that owns the responsibility; no cross-layer calls.

## 1. Layers and dependency direction

```
flinkops-api  ──►  flinkops-k8s        (kube dynamic client)
     │        ──►  flinkops-flink-rest (JobManager REST)
     │        ──►  flinkops-store      (sqlx::Any)
     │        ──►  flinkops-core       (pure logic: merge / validate / diff / health)
     └──────────►  flinkops-types      (DTO + CRD extraction + errors)
```

**Hard rules**

| Layer | May do | Must not do |
|---|---|---|
| `types` | serde structs, CRD field extraction, error enum | depend on kube / axum / sqlx |
| `core` | pure functions: baseline merge, validation, diff, health, metric-source choice | depend on axum / kube / network |
| `k8s` | read/write `flink.apache.org/v1beta1` (DynamicObject) | build HTTP responses, write to DB |
| `flink-rest` | call JobManager REST | cache, make business decisions |
| `store` | SQL and migrations | business rules (e.g. "what counts as failed") |
| `api` | routing, auth, audit, orchestration | **hold complex business logic** (push it down to `core`) |

Breaking the dependency direction is an automatic review rejection.

## 2. Module layout inside `api`

```
crates/flinkops-api/src/
├─ main.rs        boot wiring: tracing → Config → Store → Clusters → AppState → router → serve
├─ lib.rs         module declarations + re-exports
├─ config.rs      env vars → Config (with parse unit tests)
├─ state.rs       AppState (store/clusters/sessions/http) + require_user + audit
├─ error.rs       ApiError → HTTP (the single error exit)
└─ routes/
   ├─ mod.rs      route table (new endpoints: change here + the module)
   ├─ system.rs   healthz / readyz / version (no auth)
   ├─ auth.rs     login / logout / me
   ├─ jobs.rs     list/detail/metrics/log/start-stop/restart/savepoint + sync_clusters
   ├─ profiles.rs baseline read/write
   ├─ audit.rs    audit list
   ├─ sessions.rs session clusters
   └─ proxy.rs    read-only Flink UI proxy (with is_write_path unit test)
```

A new resource type gets its own `routes/<resource>.rs` — do **not** pile it into `jobs.rs`.

## 3. Naming

| Object | Rule | Example |
|---|---|---|
| crate | `flinkops-<layer>` | `flinkops-flink-rest` |
| module/file | lowercase snake_case, singular | `routes/sessions.rs` |
| handler | verb phrase matching the resource | `set_state`, `savepoint` |
| DTO | noun, suffix `View`/`Summary`/`Detail` for the view | `JobSummary` / `JobDetail` |
| error code | lowercase snake_case, stable | `readonly_proxy` / `upstream_error` |
| audit action | `resource.action`, lowercase | `job.restart` / `proxy.blocked` |
| env var | `FLINKOPS_` prefix | `FLINKOPS_CLUSTERS` |

## 4. Error handling

- Domain errors only via `flinkops_types::Error`
  (`NotFound/BadRequest/Unauthorized/Forbidden/Conflict/Upstream/Internal`).
- HTTP mapping happens once, in `error.rs`; handlers return `ApiResult<T>`.
- **No** `unwrap()` / `expect()` (except `main` and tests); never swallow a result —
  either `?` or `warn!`.
- Upstream errors must carry context:
  ```rust
  Error::Upstream { target: format!("{name} logs"), message: e.to_string() }
  ```

## 5. Handler skeleton (copy this)

```rust
pub async fn handler(
    State(st): State<Arc<AppState>>,   // 1. state
    headers: HeaderMap,                // 2. headers (auth / audit)
    Path((cluster, ns, name)): Path<(String, String, String)>,
    Query(q): Query<MyQuery>,          // or Json(body): Json<MyBody>
) -> ApiResult<Json<Value>> {
    let user = st.require_user(&headers).await?;   // 3. auth (skip for public routes)
    if st.cfg.readonly { return Err(ApiError(Error::Forbidden("read-only mode".into()))); }
    let c = st.cluster(&cluster)?;                  // 4. cluster handle
    let obj = c.get_raw(flinkops_types::crd::KIND_DEPLOYMENT, &ns, &name).await?;
    // 5. business logic (complex parts go to core)
    st.audit(&user, "job.xxx", &name, &cluster, &ns, "ok", None, None);
    Ok(Json(json!({ "ok": true })))
}
```

Extractor order is fixed: `State` → `HeaderMap` → the rest. axum needs it to recognise the handler.

## 6. Where data comes from (this is where mistakes happen)

| Data | Source | Why |
|---|---|---|
| Job list / state / conditions | **CRD** (local index) | one list call; freshness bounded by the reconcile loop (≤30s) |
| Realtime metrics / backpressure / watermark | **Flink REST** (`?get=` batching) | millisecond level; Prometheus lags 15–60s |
| History / cross-job aggregation | Prometheus (optional) | only it has history |
| Checkpoint counts | REST `/jobs/:id/checkpoints` | the CRD only stores `lastCheckpoint` |
| Logs | **pod logs first**; REST file logs need a log4j appender | K8s only keeps stdout by default |
| Who is really running | `status.conditions[Running=True]` | more trustworthy than pod readiness |

**Don't** poll `/backpressure` for "realtime": each sample walks all task threads; every 30–60s is enough.

## 7. Three rules for write operations

1. **Prefer idempotence** — restart by bumping `restartNonce`, never by deleting pods.
2. **Leave a trace** — every state-changing action writes `audit(... "ok")` or `"denied"` before returning.
3. **Read-only protection** — check both `cfg.readonly` and `Cluster.readonly` (platform + cluster level).

## 8. Testing expectations

| Layer | Expectation |
|---|---|
| `types` | CRD extraction needs fixture tests (missing status, session job, string-typed numbers) |
| `core` | at least one test per public function; merge must prove **idempotence** |
| `k8s` / `flink-rest` | no unit tests; covered by e2e (`kind` + operator chart) |
| `api` | pure functions (e.g. `is_write_path`) need unit tests; handlers covered by smoke scripts |

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 9. Frontend contract

- Backend emits camelCase only; the frontend does no conversion.
- Every list returns `{ items, count }` (or `{ items }`), never a bare array — leaves room for pagination.
- Metric endpoints must carry `source` and `degraded` so the UI can say "realtime source unavailable".
- Frontend types are generated from Rust DTOs; never hand-write a second copy.

## 10. Pre-commit checklist

1. `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
2. New endpoints are documented in `docs/api-spec.md`
3. New dependencies are in the `deny.toml` allow-list (`cargo deny check licenses`)
4. No kubeconfig / password / token in the repo (`.secrets/` is ignored)
