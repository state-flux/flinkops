# MCP Integration Spec (v1 · Contract)

**English** | [简体中文](mcp-spec.md)

> This document is a **contract**: change here first, then change the code. The MCP server and platform behaviour follow this document.
> Upstream conventions are in [`api-spec.md`](api-spec.md) (error structure, audit action naming, camelCase) and [`backend-conventions.md`](backend-conventions.md) (layering, naming, never swallow errors).

---

## 0. Scope and Non-Goals

**Do**: expose FlinkOps **operational capabilities** to AI clients (Claude Desktop / Cursor / self-built agents) in the form of MCP tools, and cage the destructive power on the server side.

**Don't**:

| Don't | Reason |
|---|---|
| Let the MCP process hold kubeconfig directly | One leak = the whole cluster can be deleted; every request must pass platform auth and audit |
| Reuse the login cookie for machine authentication | A browser session should not be handed to machines; we need tokens that are revocable and scopeable |
| Expose arbitrary Flink UI reverse-proxy paths | A universal proxy equals bypassing every structural constraint |
| Expose "submit a long-running job / create a cluster" | Boundary too large; humans do it in the web UI |
| Expose arbitrary PromQL | It would hit other systems in the cluster; only whitelisted prefixes are given |

---

## 1. Process and Transport

```
MCP host (Claude/Cursor/agent)
        │ stdio or Streamable HTTP
        ▼
flinkops-mcp (standalone binary, crates/flinkops-mcp)
        │ HTTP + Bearer token
        ▼
flinkops API (axum, the single auth and audit exit)
```

**Hard rules**

1. `flinkops-mcp` **depends only on the platform HTTP API**; it does not link `flinkops-k8s` and does not read kubeconfig.
2. Two transports, stdio by default:

| Transport | Switch | Notes |
|---|---|---|
| stdio | default | local/desktop; logs always go to stderr (stdout is reserved for protocol frames) |
| Streamable HTTP | `FLINKOPS_MCP_HTTP=1` + `FLINKOPS_MCP_TOKEN` | remote deployment; a `Host`/`Origin` allowlist is mandatory |

3. **fail-closed**: when the platform is unreachable, every tool returns `unavailable` + reason; **returning an empty list is forbidden** (an empty list is read by the model as "there are no jobs").
4. Environment variables:

| Variable | Default | Notes |
|---|---|---|
| `FLINKOPS_API_URL` | `http://127.0.0.1:8080` | platform address |
| `FLINKOPS_MCP_TOKEN` | empty (required) | API token, see §2 |
| `FLINKOPS_MCP_HTTP` | `0` | 1 = enable Streamable HTTP |
| `FLINKOPS_MCP_BIND` | `127.0.0.1:8765` | HTTP listen address |
| `FLINKOPS_MCP_ALLOWED_HOSTS` | empty | comma-separated; empty means only the bind address is allowed |
| `FLINKOPS_MCP_ALLOWED_ORIGINS` | empty | browser Origin allowlist; empty means only requests without an Origin are allowed |

---

## 2. Authentication: API Token (PAT)

### 2.1 Form

```
fko_pat_<id>_<secret>
     │      │     └─ 32 random bytes (base64url), returned only once at creation
     │      └─ 8-char short id, used for display/revocation
     └─ fixed prefix, for log redaction and leak scanning
```

The server stores only `sha256(secret)` and `id`, and **never echoes the secret back**.

### 2.2 Table structure (`MIGRATIONS` constant in `flinkops-store/src/lib.rs`)

```sql
create table mcp_token (
  id            text primary key,          -- 8-char short id
  name          text not null,
  secret_hash   text not null,             -- sha256(secret) hex
  scopes        text not null,             -- json: ["read","operate"]
  allow_jobs    text not null,             -- json: ["flink/etl-*", "flink/order-sync"]
  allow_ns      text not null,             -- json: ["flink"]
  qps           integer not null default 5,
  expires_at    text,                      -- RFC3339; null = never expires (requires admin)
  created_at    text not null,
  last_used_at  text,
  revoked_at    text
);
```

### 2.3 Scopes and resource matching

| scope | Meaning |
|---|---|
| `read` | read-only tools |
| `operate` | lifecycle actions (savepoint/restart/suspend/resume/scale/restore/image upgrade) |
| `admin` | delete jobs, change baselines/settings/alert rules |

Resource matching (**minimum by default, empty means no permission**):

- `allow_ns`: `["flink"]` or `["flink-*"]` (glob)
- `allow_jobs`: `"<namespace>/<name>"` form, `*` supported; cross-namespace targets must be listed explicitly

### 2.4 Validation order (this order every time, stop on the first failure)

```
1. token exists and not revoked / not expired   → invalid_token
2. secret_hash matches                          → invalid_token
3. scopes required by the tool ⊆ token.scopes   → denied (message states which one is missing)
4. target namespace ∈ allow_ns                  → denied
5. target job ∈ allow_jobs (or ns-level wildcard) → denied
6. tool requiring approval → check approval_token → approval_required / approval_invalid
7. rate limiting (QPS / concurrency / batch cap) → rate_limited
8. execute; write audit mcp.call
```

> **After approval, steps 3–5 must be re-executed (re-authorization)**; "approved means let through" is not allowed.

---

## 3. Tool Catalog

The naming prefix is always `flinkops_`. All tool inputs are camelCase and return JSON.

### 3.1 Read-only (scope: `read`)

| Tool | Input | Returns | Notes |
|---|---|---|---|
| `flinkops_list_jobs` | `cluster?`, `namespace?`, `state?`, `health?`, `limit?`(default 20, max 50), `cursor?` | `{items:[JobSummary], nextCursor?}` | reuses `GET /api/jobs` |
| `flinkops_get_job` | `cluster`,`namespace`,`name` | `{summary, checkpoint, savepoint}` | excludes events/logs |
| `flinkops_job_metrics` | same as above | `{jobId, values, source, sampledAt, degraded}` | when `degraded!=null` it must be passed through as-is, never treated as 0 |
| `flinkops_job_exceptions` | same as above | `{rootCause?, exceptions:[...]}` | returns only the latest 3 |
| `flinkops_job_events` | same as above, `limit?`(default 20) | `{items:[{type,reason,message,lastTimestamp}]}` | |
| `flinkops_job_yaml` | same as above | `{yaml, redacted:true, dropped:[...]}` | see §5 redaction |
| `flinkops_list_alerts` | `state?`(`OPEN`\|`ACK`), `severity?` | `{items:[AlertEvent], open}` | |
| `flinkops_list_snapshots` | `cluster?`,`namespace?`,`state?` | `{items:[Snapshot], count, unsupportedClusters?}` | |
| `flinkops_prom_query` | `query`,`rangeSec?`(default 0 = instant) | `{series:[...]}` | **whitelisted prefixes**: `flink_`, `kube_node_`, `kube_pod_`, `node_`; any other prefix → `bad_request` |

### 3.2 Lifecycle (scope: `operate`)

| Tool | Input | Approval | Returns |
|---|---|---|---|
| `flinkops_savepoint` | `cluster`,`namespace`,`name`,`path?` | No | `{mode:"cr"\|"rest", snapshot?, degraded, requestId?}` |
| `flinkops_restart` | same as above | No | `{restartNonce}` |
| `flinkops_suspend` | same as above | **Yes** | `{state:"suspended"}` |
| `flinkops_resume` | same as above | No | `{state:"running", clearedStale:[...]}` |
| `flinkops_scale` | same as above, `parallelism?`,`taskManagerReplicas?` | **Yes** | `{applied:{...}}` |
| `flinkops_restore` | same as above, `savepointPath` (**required**) | **Yes** | `{ok:true}` |
| `flinkops_upgrade_image` | same as above, `image`, `upgradeMode` (defaults to and is forced to `savepoint`) | **Yes** | `{ok:true}` |

- `restore` does not accept the implicit "latest" semantics: the AI must spell out the path so a human can review it.
- `scale` with no arguments at all → `bad_request`; with arguments that exceed the cluster's remaining resources → returns `insufficient_resources` + the shortfall (reusing the resource pre-check semantics of the submit page).

### 3.3 High-risk (scope: `admin`, **approval + second confirmation required**)

| Tool | Input | Extra requirement |
|---|---|---|
| `flinkops_delete_job` | `cluster`,`namespace`,`name`,`confirmName` | `confirmName` must match `name` exactly, otherwise `bad_request` |

### 3.4 Explicitly not exposed (written into code comments to stop future additions)

`kubeconfig` / `Secret` / `Token` / connector credentials / `POST /api/jobs` (submit) / `POST /api/clusters` (onboarding) / `PUT /api/profiles` (baseline) / any `/proxy/*`

---

## 4. Approval Flow

```
① AI: flinkops_suspend{job}
② Server: do not execute → write an approval row → return
   { "error": { "code": "approval_required",
                "message": "suspend requires human approval",
                "approvalId": "ap_7f3c…", "expiresInSec": 300 } }
③ Web "Approval Center": show tool / target / argument summary / initiating token
④ On-call approves → generate a one-time approval_token (echoed once to the operator)
⑤ AI retries the same call, Header: X-FlinkOps-Approval: <token>
⑥ Server validates: token unused, not expired, same tool, same argsHash → execute → mark as used
```

**Rules**

| Item | Value |
|---|---|
| Approval TTL | 300s |
| token | one-time; invalid immediately after use |
| argsHash | `sha256(canonical_json(args))`; approving A but executing B → `approval_invalid` |
| Who can approve | web-login users (later restricted to the `operate` role when RBAC lands) |
| After timeout | status `expired`, audit trail kept, the AI must initiate again |

Table structure:

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

## 5. Redaction Rules

`flinkops_job_yaml` and any tool that returns YAML/logs **must** be processed according to this table, and the response must carry `redacted: true` and `dropped: [...]`:

| Field | Handling |
|---|---|
| `metadata.annotations."kubectl.kubernetes.io/last-applied-configuration"` | delete the whole section |
| `metadata.managedFields` | delete the whole section |
| `spec.jobManager.podTemplate` / `spec.podTemplate` | delete the whole section |
| `initContainers[].env` | keep only `name`, `value` → `***` |
| `containers[].env` | same as above |
| `status.reconciliationStatus.lastReconciledSpec` / `lastStableSpec` | delete the whole section |
| log line matching `(?i)(password\|secret\|token\|ak\|sk\|access[_-]?key)` | replace the whole line with `***` (keep the line number) |

Log responses are capped at 200 lines; when exceeded, only the tail is returned and `truncated: true` is set.

---

## 6. Rate Limiting and Timeouts

| Item | Default | Notes |
|---|---|---|
| QPS per token | 5 | token bucket; exceeding it gives `rate_limited` with `retryAfterMs` |
| Concurrent long tasks | 1 / token | prevents concurrent savepoints from crushing HDFS |
| Batch cap | 10 jobs | any tool with batch semantics |
| Read-only timeout | 10s | |
| Write timeout | 60s | a savepoint can take a while |
| Single response body | ≤ 256KB | truncated and marked `truncated` when exceeded |

---

## 7. Audit

Every tool call writes **one** audit record (reusing the `audit` table):

```
action:  mcp.call
actor:   mcp:<token name>
target:  <namespace>/<name> or "*"
cluster: <cluster>
result:  ok | denied | error
detail:  {"tool":"flinkops_suspend","argsHash":"…","approvalId":"…","tokenId":"…","latencyMs":123,"truncated":false}
ip:      client IP
```

Read-only tools are recorded too (otherwise "what has the AI looked at" cannot be answered).

---

## 8. Error Model (fail-closed)

The MCP tool result carries `isError: true` + a structured body; the body follows the platform error structure and adds 3 new codes:

| code | When | How the AI side should read it |
|---|---|---|
| `invalid_token` | token missing/invalid/revoked | do not retry; have the user recreate the token |
| `denied` | insufficient scope or target outside the allowed range | do not retry with different arguments; tell the user exactly which permission is missing |
| `approval_required` | approval needed (carries `approvalId`) | ask the user to approve in the web UI, then retry with `X-FlinkOps-Approval` |
| `approval_invalid` | token used/expired/arguments mismatched | initiate a new approval |
| `rate_limited` | QPS/concurrency exceeded | back off according to `retryAfterMs` |
| `insufficient_resources` | scale/submit exceeds cluster headroom | report the shortfall, suggest lowering parallelism |
| `unavailable` | platform unreachable / JobManager REST unreachable | do **not** read it as "no data" |

The remaining codes follow the code table in [`api-spec.md §1`](api-spec.md).

---

## 9. New Platform-Side Endpoints (companions to this contract)

### POST /api/tokens

```json
{ "name": "claude-desktop", "scopes": ["read","operate"],
  "allowNamespaces": ["flink"], "allowJobs": ["flink/ant-*"],
  "qps": 5, "expiresInDays": 30 }
```
→ `201 { "id":"a1b2c3d4", "token":"fko_pat_a1b2c3d4_<secret>", "note":"shown only once" }`

### GET /api/tokens
→ `{ items:[{id,name,scopes,allowNamespaces,allowJobs,qps,expiresAt,lastUsedAt,revokedAt}] }` (**no secret**)

### DELETE /api/tokens/{id}
→ `200 {"ok":true}`; writes a `token.revoke` audit record

### GET /api/approvals?status=pending
→ `{ items:[{id,tool,target,argsHash,actor,tokenName,createdAt,status}] }`

### PATCH /api/approvals/{id}
```json
{ "decision": "approve" }   // or "reject"
```
→ `200 { "ok":true, "approvalToken":"…" }` (returned only once, on approve)
→ writes an `approval.decide` audit record

> New audit actions: `mcp.call`, `token.create`, `token.revoke`, `approval.decide`. Naming follows `resource.action`.

---

## 10. Frontend

- Settings page gains an "MCP and Tokens" section: token list (name/scope/range/last used/revoke), creation wizard (scope checkboxes + ns/job globs + QPS + TTL)
- A new page, or one next to the alert center, called "Approval Center": pending list (tool/target/argument summary/initiator) + approve/reject; after approval the approval token is shown once
- Job detail page: if the job has a pending approval, show the banner "AI requested to suspend this job, awaiting approval"

---

## 11. Versioning and Compatibility

- The `flinkops_mcp_version` tool returns `{ "api":"1", "tools":"1", "platform":"0.1.0" }`
- Breaking changes (renaming a parameter, removing a tool, restricting a response) → `tools` +1
- On startup the MCP server verifies that `/api/version` is inside the supported range; on mismatch all tools return `unavailable` with a description of the version gap

---

## 12. Test Matrix (acceptance cases, all mandatory)

| # | Scenario | Expected |
|---|---|---|
| 1 | read-only token calls `flinkops_suspend` | `denied`, message states the missing `operate`; audit `result=denied` |
| 2 | `operate` token suspends `flink/order-sync` (out of range) | `denied` |
| 3 | `operate` token suspends an in-range job with no approval | `approval_required` + `approvalId` |
| 4 | approve job A, then change arguments to suspend job B | `approval_invalid` (argsHash mismatch) |
| 5 | use the same approval token twice | second time `approval_invalid` |
| 6 | `confirmName` mismatch when deleting a job | `bad_request`, nothing executed |
| 7 | platform down | all tools `unavailable`, **not an empty list** |
| 8 | `flinkops_prom_query` with `up{job="x"}` | `bad_request` (prefix not whitelisted) |
| 9 | grep `password\|secret\|AK\|SK` in the YAML tool response | 0 hits, and `dropped` lists the removed fields |
| 10 | 20 consecutive calls (qps=5) | `rate_limited` triggered, visible in audit |
| 11 | call immediately after the token is revoked | `invalid_token` |
| 12 | job detail after a suspend (REST unreachable) | `flinkops_get_job` still returns summary, the checkpoint section is marked degraded |

---

## 13. Implementation Checklist (file level)

```
crates/flinkops-mcp/
├─ Cargo.toml
├─ src/main.rs         both transports + fail-closed + logs to stderr
├─ src/client.rs       platform API client (Bearer, timeouts, error mapping)
├─ src/auth.rs         token parsing/local cache/scope checks (server is still the final arbiter)
├─ src/tools/read.rs   §3.1
├─ src/tools/operate.rs§3.2 (approval gate first)
├─ src/tools/admin.rs  §3.3 (second confirmation)
├─ src/redact.rs       §5
├─ src/errors.rs       §8
└─ tests/              §12 cases 1–12

crates/flinkops-api/src/routes/
├─ tokens.rs           POST/GET/DELETE /api/tokens
├─ approvals.rs        GET/PATCH /api/approvals + GET /api/approvals/stream (SSE)
└─ mcp_guard.rs        scope/range/rate-limit/approval checks (shared by tokens and the write endpoints)

crates/flinkops-store/src/lib.rs
└─ MIGRATIONS constant flinkops_mcp_tokens / flinkops_mcp_approvals (inline DDL like the other tables,
                        there is no separate migrations/ directory)

docs/
├─ mcp-spec.md (this file)
└─ api-spec.md (append §10 MCP companion endpoints)
```

**Implementation order**: `tokens.rs` → `mcp_guard.rs` → `approvals.rs` → `flinkops-mcp` read-only tools → operate + approval → admin → the two frontend panels → run §12.

---

## 14. Implementation Status (v1 shipped, 2026-11-26)

| Contract item | Landing site | Status |
|---|---|---|
| §2.1 Token form `fko_pat_<id>_<secret>`, only sha256 stored | `crates/flinkops-api/src/mcp_guard.rs` (`bearer_parts` / `sha256_hex`), `routes/tokens.rs` | ✅ |
| §2.2 scope: read / operate / admin | `mcp_guard::scope_ok`; write endpoints take operate by tool name, delete takes admin | ✅ |
| §2.3 resource range allow_ns / allow_jobs (empty = no permission, `*` glob supported) | `mcp_guard::job_allowed` + `check_job_scope`; an "empty range" is rejected at creation time | ✅ |
| §2.4 QPS token bucket | `AppState::mcp_rate` + `mcp_guard::take_rate`; over-limit returns 429 (no longer disguised as 401) | ✅ |
| §4 one-time approval (300s window, argsHash binding, single-use redemption) | `flinkops_mcp_approvals` + `mcp_guard::gate_write` + `routes/approvals.rs` | ✅ |
| §5 redaction (YAML section deletion + env masking + log line masking) | `crates/flinkops-mcp/src/redact.rs` (with unit tests) | ✅ |
| §3 tool surface of 17 (9 read / 7 operate / 1 admin) | `crates/flinkops-mcp/src/tools.rs` | ✅ |
| §6 audit `token.create` / `token.revoke` / `approval.decide` | `routes/tokens.rs`, `routes/approvals.rs` | ✅ |
| §7 error codes approval_required / approval_invalid / denied / unavailable | `gate_write` returns `Conflict`/`Forbidden`; the MCP side passes them through to the model verbatim | ✅ |
| §9 platform endpoints `/api/tokens`, `/api/approvals` | `crates/flinkops-api/src/routes/mod.rs` | ✅ |
| Frontend: token management + approval center | `apps/web/src/views/Mcp.vue` (`/mcp`) | ✅ |
| Realtime approval push (SSE, no polling) | `GET /api/approvals/stream` + `AppState::approvals_tx` (broadcast); frontend EventSource + toast | ✅ |
| §11 HTTP transport (Streamable HTTP) | crates/flinkops-mcp/src/main.rs (--http host:port, POST /mcp accepts a single message or a batch, GET / self-description, GET /healthz) | ✅ |

### Measured (2026-11-26, 8081 instance)

```
token creation        → fko_pat_e6ab09c3_74678…
mcp --check           → ok: mcp:mcp-e2e @ http://127.0.0.1:8081
tools/list            → 17
list_jobs             → count=3 truncated=true
prom whitelist        → 400 bad_request (mysql_global_status_connections)
out of range models/… → 403 forbidden
scale, no approval    → 409 approval_required:ap_f99cfc6d5d23
approved in center    → approvalToken=apv_2cf5b9b408…
retry with approval   → 400 bad_request (parallelism cannot be 0; the gate was passed, the job untouched)
same token reused     → 403 approval_invalid (one-time only)
--check after revoke  → 401 unauthorized
```
