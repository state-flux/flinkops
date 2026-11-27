# API Spec (v1)

**English** | [简体中文](api-spec.md)
> This document is the **contract**. Change it here before changing code for any new or modified endpoint; the implementation must match this document, and anything not described here is unspecified.

- Base path: same origin, no prefix (`/api/...`)
- Auth: `HttpOnly` cookie `flinkops_session` (issued by the server after login)
- Content type: requests and responses are both `application/json; charset=utf-8` (except logs and transparent reverse-proxy passthrough)
- Time: RFC 3339 UTC, e.g. `2026-11-25T02:01:33Z`
- Field naming: **always camelCase** (Rust side uses `#[serde(rename_all = "camelCase")]`)
- Numbers: concurrency, replicas, slots etc. are integers; metric values are floats

## 1. Unified errors

HTTP status code + fixed structure:

```json
{ "error": { "code": "not_found", "message": "cluster prod 未接入" } }
```

| code | HTTP | When |
|---|---|---|
| `unauthorized` | 401 | Not logged in / session expired |
| `forbidden` | 403 | Read-only mode, read-only cluster, reverse-proxy write operations |
| `not_found` | 404 | Cluster/CRD/job does not exist |
| `bad_request` | 400 | Invalid parameters, metrics called without ids |
| `conflict` | 409 | Job has no JobID yet, concurrency conflict |
| `upstream_error` | 502 | kube-apiserver / JobManager REST failure |
| `internal` | 500 | Other |

**Do not** add other top-level structures; **do not** return the raw upstream error text directly as message (carry context, see `flinkops_types::Error::Upstream`).

## 2. Authentication and sessions

### POST /api/login

```json
{ "username": "admin", "password": "admin" }
```
→ `200` + `Set-Cookie: flinkops_session=<uuid>; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400`

Failure → `401 {"error":{"code":"bad_credentials",...}}`, and one `auth.login / denied` audit entry is written.

### POST /api/logout
Clears the cookie → `200 {"ok":true}`

### GET /api/me
```json
{ "user": "admin", "readonly": false, "clusters": ["prod"], "prometheus": false }
```
`readonly` decides whether the frontend hides write buttons; when `prometheus=false` the dashboard shows no history.

## 3. Jobs

### GET /api/jobs
Query: `cluster?`, `namespace?`, `refresh?` (when `true`, sync once from K8s first)

```json
{
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

- `health` is computed by the backend: `green` (`Running=True`) / `yellow` (state RUNNING but conditions unconfirmed, or reconciling) / `red` (FAILED or `status.error` non-empty)
- List data comes from the **local index** (a materialized view of the CRD), so `state` lags by up to one reconcile cycle (30s by default)

### POST /api/jobs/sync
No body. → `200 {"ok":true,"synced":29}`. Pulled cluster by cluster; a single cluster failure only warns and does not fail the whole call.

### GET /api/jobs/{cluster}/{ns}/{name}
Returns `JobDetail`: `summary` / `spec` / `checkpoint` / `savepoint` / `taskManager` / `clusterInfo`

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

`checkpoint.completed/failed` come from Flink REST; 0 when unavailable (details still work, no error).

### GET /api/jobs/{cluster}/{ns}/{name}/metrics
Query: `ids?` (comma-separated, defaults to the 10 in `LIVE_METRICS`)

```json
{ "jobId": "9f2c…", "values": { "numRecordsInPerSecond": 32412 }, "source": "flink-rest", "sampledAt": "…", "degraded": null }
```

- When the realtime source fails it returns `source:"none"|"prometheus"` + `degraded:"<reason>"` with `values` empty — **the frontend must show this explicitly and must not treat it as 0**
- Job has no JobID yet → `409 conflict`
- Empty `ids` is forbidden (it scans all metrics and slows down the JM)

### GET /api/jobs/{cluster}/{ns}/{name}/log
Query: `kind=main|stdout` → `200 text/plain`

REST file logs do not exist (K8s only outputs stdout by default) → `502 upstream_error`, and the message says "use Pod logs".

### POST /api/jobs/{cluster}/{ns}/{name}/state
```json
{ "running": false }
```
→ `200 {"ok":true,"state":"suspended"}`; platform read-only → `403`; writes audit `job.stop` / `job.start`.

### POST /api/jobs/{cluster}/{ns}/{name}/restart
No body. Bumps `spec.restartNonce` (recreates the JM, state untouched) → `200 {"ok":true,"restartNonce":4}`.

### POST /api/jobs/{cluster}/{ns}/{name}/savepoint
```json
{ "path": "s3://flink-state/savepoints/orders" }
```
Creates a `FlinkStateSnapshot` (instead of hitting REST directly) → `200 {"ok":true,"snapshot":"orders-sp-20261125020133"}`.

## 4. Platform

### GET /api/profiles
```json
{ "items": [ { "id": "general-1.19", "name": "general-1.19", "flinkVersion": "v1_19", "image": "…", "namespace": "flink-jobs", "checkpointPrefix": "s3://flink-state/checkpoints/{name}", "savepointPrefix": "…", "stateBackend": "rocksdb", "restExposed": "ClusterIP", "metricsPort": 9250, "serviceAccount": "flink-sa", "defaults": {…}, "labels": {…}, "builtin": true } ] }
```

### PUT /api/profiles/{id}
Body is a complete `Profile` (`id` may be omitted, the path wins; `builtin` is forced to false).
→ `200 {"ok":true,"findings":[Finding…]}`, where `findings` are the validation results of generating a spec from that baseline and can be rendered directly.

### GET /api/audit
→ `{"items":[{ "id","ts","actor","action","target","cluster","namespace","result","detail" }]}`, newest first, capped at 200 entries.

Audit `action` naming (must be followed when adding new ones):
`auth.login` / `jobs.sync` / `job.start` / `job.stop` / `job.restart` / `savepoint.trigger` / `profile.save` / `ui.open` / `proxy.blocked`

`result`: `ok` | `denied` | `error`

### GET /api/sessions
Query: `cluster?`
```json
{ "items": [ { "cluster","namespace","name","state","flinkVersion","image","jobManagerCpu","jobManagerMemory","taskManagerReplicas","slotsPerTm","usedSlots","idleTtl","sqlGateway" } ] }
```
Returns only `FlinkDeployment`s with `spec.mode=session`.

## 5. Flink UI read-only reverse proxy

```
ANY /proxy/{cluster}/{ns}/{name}/{*path}
```

| Rule | Behavior |
|---|---|
| `GET/HEAD/OPTIONS` | Pass through to `http://{name}-rest.{ns}.svc.cluster.local:8081/{path}`, rewrite `Location` |
| `PATCH ?mode=cancel`, `*/stop`, `*/savepoints`, `*/jars*`, `*/rescaling`, `DELETE *` | `403 {"code":"readonly_proxy"}`, writes a `proxy.blocked` audit entry |
| `Upgrade: websocket` | `501 {"code":"ws_not_supported"}` (UI degrades to polling) |
| Not logged in | `401` |

**The decision must look at both path and query** (`?mode=cancel` lives in the query; looking only at the path misses it).

## 5.1 Submitting jobs (new)

### POST /api/jobs

Body accepts two forms (either one):

```json
{ "yaml": "apiVersion: flink.apache.org/v1beta1\nkind: FlinkDeployment\n…" }
```
```json
{ "object": { "apiVersion": "…", "kind": "FlinkDeployment", "metadata": {…}, "spec": {…} } }
```

Query: `dryRun=true` only validates and checks for a duplicate name, **nothing is written to the cluster** (for integration testing and pre-checks).

Processing order (the order must not change):
1. Parse YAML/JSON → must be a `FlinkDeployment` (use the `/api/sessions` family for `FlinkSessionJob`)
2. `core::validate(spec)`; any `error`-level finding returns `400` directly, with `findings` in the response body
3. Check whether an object with the same name already exists → `409 conflict`
4. `k8s.apply` (skipped for dryRun)
5. Write audit `job.submit`, with `image` and `parallelism` in detail

```json
{ "ok": true, "name": "orders-etl", "namespace": "flink-jobs", "dryRun": false }
```

Errors:

| Scenario | HTTP | code |
|---|---|---|
| YAML parse failure | 400 | `bad_request` |
| kind is not FlinkDeployment | 400 | `bad_request` |
| Validation has errors | 400 | `bad_request` (response contains `findings`) |
| Same name already exists | 409 | `conflict` |
| Platform read-only | 403 | `forbidden` |

### Sibling endpoints of POST /api/jobs/{cluster}/{ns}/{name}/state

| Method | Path | Description |
|---|---|---|
| POST | `/api/jobs/{c}/{ns}/{name}/restart` | bump `spec.restartNonce` |
| POST | `/api/jobs/{c}/{ns}/{name}/savepoint` | create `FlinkStateSnapshot` |

## 5.2 Events and exceptions (new)

### GET /api/jobs/{cluster}/{ns}/{name}/events

Reads `core/v1 Event`, filtered by `involvedObject.name` + `kind=FlinkDeployment` (the fieldSelector is done server-side to avoid pulling everything).

```json
{ "items": [ { "type": "Warning", "reason": "BackOff", "message": "…", "count": 7,
               "firstTimestamp": "2026-11-25T01:00:00Z", "lastTimestamp": "2026-11-25T02:00:00Z",
               "source": "kubelet" } ], "count": 1 }
```

### GET /api/jobs/{cluster}/{ns}/{name}/exceptions

Passes through Flink REST `/jobs/:id/exceptions`:

```json
{ "rootException": "java.lang.OutOfMemoryError: Java heap space", "truncated": false, "exceptions": [] }
```

Job has no JobID → `409`; REST unreachable → `502` (the message says "use the Flink UI or Pod logs").

## 5.3 Snapshot list (new)

### GET /api/snapshots

Query: `cluster?`, `namespace?`, `state?` (`COMPLETED|IN_PROGRESS|FAILED`)

Lists `flinkstatesnapshots`, newest first:

```json
{ "items": [ { "cluster": "prod", "namespace": "flink-jobs", "name": "orders-sp-20261125020133",
               "jobName": "orders", "state": "COMPLETED", "path": "s3://flink-state/savepoints/orders/…",
               "createdAt": "2026-11-25T02:01:33Z" } ], "count": 1 }
```

`state` returns `UNKNOWN` when unknown (no guessing).
## 5.4 Creating session clusters (new)

### POST /api/sessions

```json
{ "name": "flink-sql-session", "cluster": "prod", "namespace": "flink-jobs",
  "profileId": "general-1.19", "slots": 4, "taskManagerReplicas": 2, "dryRun": false }
```

Behavior: render the spec from the "environment baseline" + overrides → set `spec.mode=session` → drop the `job` section → validate → duplicate-name check → apply.
Audits `session.create`.

```json
{ "ok": true, "cluster": "prod", "namespace": "flink-jobs", "name": "flink-sql-session", "dryRun": false }
```

Session mode **does not require** `job.jarURI` / `job.parallelism` (`core::validate` already relaxes per `spec.mode`, with unit-test coverage).

## 6. Five steps for a new endpoint

1. **Write the doc**: add a section to this document (path, method, request, response, errors, permissions, audit action).
2. **Define the DTO**: put it in `flinkops-types` (`camelCase` + `cargo test` covering extraction/serialization).
3. **Write the handler**: in `crates/flinkops-api/src/routes/<resource>.rs`;
   - Fixed signature: `State<Arc<AppState>>` first, `HeaderMap` second, `Path`/`Query`/`Json` after;
   - First line: `let user = st.require_user(&headers).await?;` (except public endpoints);
   - Write operations: check `st.cfg.readonly` first, then execute, then `st.audit(...)`.
4. **Register the route**: add `.route(...)` in `routes/mod.rs` (axum 0.8 uses `{param}` syntax).
5. **Add tests**: plain-logic unit tests in `flinkops-core`; `routes/proxy.rs` already has `is_write_path` unit tests to follow.

Template:

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

## 7. Not implemented yet (roadmap)

| Capability | Notes |
|---|---|
| `POST /api/jobs` (submit) | Needs `core::spec_from_profile` + `k8s.apply`; the logic already exists |
| `POST /api/jobs/{…}/validate`, `/diff` | `core::validate` / `core::diff` are implemented with unit tests, only exposure is missing |
| `GET /api/prometheus/query` | Pass through PromQL (used when `metrics.backend=prometheus`) |
| `GET/POST /api/snapshots` | `FlinkStateSnapshot` list and create (`k8s.create_savepoint` already exists) |
| `POST /api/sessions` | Create session clusters (needs `core` to generate the spec) |
| OIDC login | Currently only local accounts + in-memory sessions |
| WebSocket reverse proxy | Needs `hyper::upgrade` + `tokio-tungstenite` |

## 8. Endpoint table (in sync with the implementation)

> This section is the "quick reference". For detailed fields see the sections above; new endpoints must be registered here.

### 8.1 System and auth

| Method | Path | Description |
|---|---|---|
| GET | `/healthz` | Liveness (no login) |
| GET | `/readyz` | Readiness: DB readable + at least one cluster (no login) |
| GET | `/api/version` | Version / cluster count / metrics source (no login) |
| POST | `/api/login` | `{username,password}` → HttpOnly cookie |
| POST | `/api/logout` | Clear cookie |
| GET | `/api/me` | Current user / cluster list / readonly / prometheus |

### 8.2 Jobs

| Method | Path | Description |
|---|---|---|
| GET | `/api/jobs` | List; `?cluster=&namespace=&refresh=true` |
| POST | `/api/jobs` | Submit a FlinkDeployment; `?cluster=&dryRun=true` |
| POST | `/api/jobs/sync` | Full sync (including orphan index cleanup) |
| GET | `/api/jobs/{c}/{ns}/{n}` | Details |
| DELETE | `/api/jobs/{c}/{ns}/{n}` | Delete a job |
| GET | `…/metrics` | REST realtime metrics (`?ids=`) |
| GET | `…/log` | JM logs (`?kind=main\|stdout`) |
| GET | `…/yaml` | Raw CRD YAML |
| GET | `…/events` | K8s events (fieldSelector filtered) |
| GET | `…/exceptions` | Flink root-cause exception |
| GET | `…/backpressure` | Vertex backpressure (`?vertex=`, once every 30–60s) |
| GET | `…/flamegraph` | Vertex flame graph Traces JSON (`?type=FULL\|ON_CPU\|OFF_CPU`) |
| POST | `…/profile` | async-profiler (Flink 1.18+) |
| POST | `…/state` | `{running:bool}` start/stop |
| POST | `…/restart` | bump `restartNonce` |
| POST | `…/savepoint` | Create a FlinkStateSnapshot (`{path?}`) |
| POST | `…/restore` | Restore from a savepoint (`{path?}`) |
| POST | `…/scale` | `{parallelism?, taskManagerReplicas?}` |
| POST | `…/image` | `{image, upgradeMode}` rolling upgrade |
| POST | `…/ingress` | `{host, className?}` create a direct domain |
| POST | `…/autoscaler` | `{enabled}` toggle the autoscaler |

### 8.3 Session clusters / snapshots / templates

| Method | Path | Description |
|---|---|---|
| GET | `/api/sessions` | Session cluster list (criterion: `spec` has no `job` section) |
| POST | `/api/sessions` | Create a `mode=native` FlinkDeployment with no `job` |
| GET | `/api/snapshots` | FlinkStateSnapshot list; returns `unsupportedClusters` when the CRD is missing |
| DELETE | `/api/snapshots/{c}/{ns}/{n}` | Delete a snapshot |
| GET | `/api/templates` | Template list |
| POST | `/api/templates` | `{name, fromJob?}` or `{name, payload}` (**auto-redacted**) |
| DELETE | `/api/templates/{id}` | Delete a template |

### 8.4 Platform governance

| Method | Path | Description |
|---|---|---|
| GET | `/api/profiles` / PUT `/api/profiles/{id}` | Environment baseline |
| GET | `/api/audit` | Audit (latest 200 entries) |
| GET | `/api/cluster-info` | operator image + CRD capabilities + stale status count |
| POST | `/api/clusters` | Add a cluster dynamically (persisted to the DB) |
| DELETE | `/api/clusters/{id}` | Remove a dynamic cluster (ones from env are refused) |
| POST | `/api/clusters/{id}/reconnect` | Rebuild the client using the original connection method |
| GET | `/api/clusters/{id}/credential` | Connection info (**redacted, no private key**) |
| GET | `/api/settings` / POST `/api/settings` | Runtime configuration (retention / DB cap / Prometheus) |
| POST | `/api/settings/reset` | Restore defaults |
| GET | `/api/admin/diagnostic` | Diagnostic bundle (safe to share) |
| GET | `/api/admin/backup` | Backup info |
| POST | `/api/admin/vacuum` | SQLite VACUUM |

### 8.5 Alerts

| Method | Path | Description |
|---|---|---|
| GET | `/api/alerts/rules` | Rule list + optional kind |
| POST | `/api/alerts/rules` | Create/update a rule |
| DELETE | `/api/alerts/rules/{id}` | Delete a rule |
| GET | `/api/alerts/events` | Events (`?state=OPEN\|ACK`) |
| POST | `/api/alerts/events/{id}/ack` | Acknowledge an event |

Rule kinds: `job_not_running` / `restarts_high` / `nodeport_exposed` / `slots_over`; evaluated in the background every 60s, only one OPEN entry is kept per rule + job.

### 8.6 Monitoring and reverse proxy

| Method | Path | Description |
|---|---|---|
| GET | `/api/metrics/query` | Instant PromQL query |
| GET | `/api/metrics/query_range` | Range query (history curves) |
| GET | `/api/prometheus/discover` | Auto-discover candidate Services |
| POST | `/api/observability/podmonitor` | apply PodMonitor (`{yaml}`) |
| POST | `/api/observability/patch-labels` | Add metadata.labels to a job |
| ANY | `/proxy/{c}/{ns}/{n}` and `…/{*path}` | Flink UI read-only reverse proxy (including `/ws`) |

## 9. Request body field rules (pitfalls we have hit)

**All request body structs must carry `#[serde(rename_all = "camelCase")]`.**

Omitting it causes: the frontend sends `upgradeMode` / `taskManagerReplicas` / `auditRetentionDays`,
the backend field is `upgrade_mode` etc. → deserialized as `None` → defaults are used → **the endpoint returns 200 but the value never takes effect**.
This pitfall has been hit 3 times in this project (ImageBody, ScaleBody, SaveBody) and is now a mandatory review check.

## 10. MCP companion endpoints (see [`mcp-spec.md`](mcp-spec.md))

Machine auth and approval endpoints required for MCP integration; the contract details are in `mcp-spec.md §9` — only the paths are registered here so the summary table stays complete:

| Method | Path | Description |
|---|---|---|
| POST | `/api/tokens` | Create an API Token (the secret is returned only once) |
| GET | `/api/tokens` | Token list (no secrets) |
| DELETE | `/api/tokens/{id}` | Revoke a Token |
| GET | `/api/approvals` | Pending approvals (`?status=pending`) |
| PATCH | `/api/approvals/{id}` | Approve/reject; approval returns a one-time approval token |

New audit actions: `mcp.call`, `token.create`, `token.revoke`, `approval.decide` (following the `resource.action` naming).
MCP-side error codes are in `mcp-spec.md §8` (`invalid_token` / `denied` / `approval_required` / `approval_invalid` / `rate_limited` / `insufficient_resources` / `unavailable`).
