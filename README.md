# FlinkOps

**English** | [简体中文](README.zh-CN.md)

An operations console for [Apache Flink](https://flink.apache.org/) on Kubernetes:
**one Rust binary** (axum) that also serves the **Vue 3 SPA**. Metadata goes to
SQLite, PostgreSQL or MySQL, chosen by `FLINKOPS_STORE` (the store rewrites `?` placeholders per dialect before sending, since `sqlx::Any` does not); metrics are never persisted.

It drives `FlinkDeployment` / `FlinkSessionJob` / `FlinkStateSnapshot` through the
[Flink Kubernetes Operator](https://github.com/apache/flink-kubernetes-operator) —
it is an operations surface, not a job-submission or scheduling platform.

> Design contracts live in [`docs/backend-conventions.md`](docs/backend-conventions.md)
> and [`docs/api-spec.md`](docs/api-spec.md). New endpoints: change the doc first, then
> the code — so that the doc and the implementation cannot drift apart.

---

## Why

Running Flink on Kubernetes means living with:

- job lifecycle through CRDs (`savepointTriggerNonce`, `restartNonce`, `upgradeMode`) while
  the Flink Web UI only knows a single cluster per port-forward;
- 40+ deployments and no answer to "which one is unhealthy right now, and why";
- a Web UI that happily lets anyone cancel a production job;
- no audit trail for "who restarted this at 3am".

FlinkOps puts a thin, auditable layer in front of all of it: lifecycle actions land on the
CRD, every action writes an audit row, and the Flink Web UI is reverse-proxied read-only.

## What you get

- **Job lifecycle** — start / suspend / restart / savepoint / restore / scale / image
  upgrade / ingress / autoscaler, all through the CRD, all audited.
- **Read-only Flink UI proxy** — HTML, static assets and `/ws` are tunneled; cancel, stop,
  savepoint and rescale are rejected at the proxy layer with `403` and an audit row.
- **Observability** — Prometheus pass-through behind a metric-name allow-list, step
  auto-scaling, per-operator hotspot table, back-pressure heat map.
- **Alerts** — threshold rules evaluated every 60s; "NodePort exposed" only fires when the
  platform itself runs in-cluster (otherwise NodePort is the only way in).
- **MCP** — `flinkops-mcp` exposes 17 tools to AI clients with scope → resource allow-list →
  QPS → one-shot human approval. See [`docs/mcp-spec.md`](docs/mcp-spec.md).
- **Multi-cluster** — several clusters via `id=incluster` or `id=/path/kubeconfig`.

## Screenshots

> Demo data — job names, images, cluster and storage paths are redacted.

| | |
|:--:|:--:|
| ![Overview](docs/screenshots/dashboard.png) | ![Jobs](docs/screenshots/jobs.png) |
| **Overview** — KPIs, resource watermarks and state distribution at a glance | **Jobs** — 51 resources synced in real time: throughput, backpressure, slots, resources |
| ![Job detail](docs/screenshots/job-detail.png) | ![Metrics](docs/screenshots/metrics.png) |
| **Job detail** — lifecycle actions, checkpoints, diagnostics, config and YAML on one page | **Metrics** — Prometheus + Flink REST, throughput, watermark lag, checkpoint duration |
| ![Checkpoints](docs/screenshots/savepoints.png) | ![CRD editor](docs/screenshots/editor.png) |
| **Checkpoints & snapshots** — snapshot list and per-job distribution, one-click restore | **CRD editor** — server-side validation, diff against the cluster, save with resourceVersion CAS |

## Layout

```
crates/                        Rust backend
├─ flinkops-types/             DTOs + CRD field extraction + errors (no kube/axum)
├─ flinkops-tls/               pure-Rust TLS provider (rustls + RustCrypto) — no C toolchain needed to cross-build
├─ flinkops-core/              pure logic: baseline merge / validation / diff / health
├─ flinkops-store/             sqlx::Any → SQLite / PostgreSQL (dialect-aware placeholders)
├─ flinkops-k8s/               kube dynamic client (flink.apache.org/v1beta1)
├─ flinkops-flink-rest/        JobManager REST (metrics / logs / backpressure / profiler)
├─ flinkops-api/               axum control plane
├─ flinkops-mcp/               MCP server (stdio + Streamable HTTP)
└─ flinkops-cli/               flinkopsctl (doctor / jobs / export)

apps/web/                      Vue 3 + TS + Tailwind v4 + shadcn-vue + CodeMirror 6
docs/                          API contract, conventions, MCP spec, screenshots
deploy/                        Dockerfile (frontend build + distroless) + Helm chart
```

`main.rs` is wiring only: `tracing → Config → Store → Clusters → AppState → router → serve`.

---

## Quick start

### Local (against an existing cluster)

```bash
export FLINKOPS_CLUSTERS="prod=$HOME/.kube/config"   # or id=incluster
export FLINKOPS_STORE="sqlite://./data/flinkops.db?mode=rwc"
export FLINKOPS_WEB_DIR="./apps/web/dist"            # run the web build first

cargo run -p flinkops-api --bin flinkops             # http://127.0.0.1:8080  admin/admin
```

On Windows the SQLite DSN needs **three slashes**:
`sqlite:///C:/flinkops/data/flinkops.db?mode=rwc`.

### Run from an IDE (zero config)

Drop your kubeconfig at `.secrets/kubeconfig.yaml` and just hit Run — no environment
variables needed:

1. `.env` is loaded when present (never overrides existing variables);
2. without `FLINKOPS_CLUSTERS`, `.secrets/kubeconfig.yaml` is used, falling back to `incluster`;
3. relative paths (web dir, SQLite DSN) are resolved by walking **up** to the workspace root.

### Frontend dev

```bash
cd apps/web && npm install && npm run dev   # :5173, /api and /proxy proxied to :8080
```

### Release artifacts

Tagging runs the multi-platform build on native runners; every artifact ships with `SHA256SUMS.txt`:

| Platform | Artifact | Install |
|---|---|---|
| Linux x86_64 / arm64 | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Debian / Ubuntu | `flinkops_<ver>_amd64.deb` / `_arm64.deb` | `dpkg -i`, systemd unit |
| macOS Intel / Apple Silicon | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Windows x86_64 | `.zip` (portable) / `-setup.exe` (installer) | unzip and run `flinkops.exe`; installer needs no admin |
| Source (any platform) | `-src.tar.gz` / `-src.zip` | unpack, `cargo build --release` |

Each archive holds `bin/{flinkops,flinkopsctl,flinkops-mcp}` + `web/` + env sample and systemd unit.
Release runbook (version alignment, artifact matrix, packager flags, rollback): [`docs/release.en.md`](docs/release.en.md).

### Docker

```bash
docker run -d --name flinkops -p 8080:8080 -v flinkops-data:/data \
  -v ~/.kube/config:/etc/flinkops/kubeconfig:ro \
  -e FLINKOPS_CLUSTERS="prod=/etc/flinkops/kubeconfig" \
  ghcr.io/state-flux/flinkops:0.1.0
```

Or build it yourself: `docker build -f deploy/Dockerfile -t flinkops:dev .`

### Kubernetes / k3s

Published chart (OCI, same version as the image):

```bash
helm install flinkops oci://ghcr.io/state-flux/charts/flinkops \
  --version 0.1.0 -n flinkops --create-namespace \
  --set store.storageClass=local-path \
  --set ingress.enabled=true --set ingress.className=traefik   # nginx on standard K8s
```

From a checkout: `helm install flinkops deploy/helm/flinkops -n flinkops --create-namespace`

> Deploying **inside** the cluster (`clusters: "local=incluster"`) is the only way to use
> `ClusterIP` REST services; from outside you must keep `NodePort` (or port-forward).

---

## Metadata backends

`FLINKOPS_STORE` decides where metadata lives. All three backends are exercised by `store_smoke`
(migration idempotency, insert, filtered query, upsert):

| Backend | DSN example | Notes |
|---|---|---|
| SQLite | `sqlite:///abs/path/flinkops.db?mode=rwc` | default, zero dependencies; single writer, keep one replica |
| PostgreSQL | `postgres://user:pass@host:5432/flinkops` | best choice when several replicas share metadata |
| MySQL | `mysql://user:pass@host:3306/flinkops?charset=utf8mb4` | supported; long text columns map to `MEDIUMTEXT`, read via bytes→utf8 |

Dialect differences live in `crates/flinkops-store/src/dialect.rs`: placeholders (Postgres `$1`),
DDL (MySQL TEXT cannot be a key or carry a default), and the MySQL quirk where `sqlx::Any` reports
`TEXT` as `BLOB`.

---

## Environment variables

| Variable | Default | Notes |
|---|---|---|
| `FLINKOPS_BIND` | `0.0.0.0:8080` | listen address |
| `FLINKOPS_CLUSTERS_DIR` | SQLite 同级的 `clusters/` | 粘贴方式接入时 kubeconfig 落盘位置（目录 0700 / 文件 0600） |
| `FLINKOPS_STORE` | `sqlite://./data/flinkops.db?mode=rwc` | also `postgres://…` and `mysql://…` (see “Metadata backends”) |
| `FLINKOPS_COOKIE_SECURE` | unset = auto | `true`/`false`；auto 按 `X-Forwarded-Proto` 判断是否给 Cookie 加 `Secure` |
| `FLINKOPS_CLUSTERS` | `local=incluster` | `id=incluster` or `id=/path/kubeconfig`, comma separated |
| `FLINKOPS_WEB_DIR` | `./apps/web/dist` | frontend build output |
| `FLINKOPS_READONLY` | `false` | platform-level read-only (rejects write APIs) |
| `FLINKOPS_PROMETHEUS_URL` | empty | empty = REST-only realtime data, no history |
| `FLINKOPS_ADMIN_USER` / `_PASSWORD` | `admin` / `admin` | use a Secret in production |
| `FLINKOPS_AUDIT_RETENTION_DAYS` | `90` | audit retention |
| `FLINKOPS_DB_MAX_MB` | `512` | DB size warning threshold |

Keep kubeconfigs in `.secrets/` (already git-ignored). See [`.env.example`](.env.example).

---

## MCP: let AI clients operate Flink safely

The machine never holds cluster credentials — only a token issued by the platform. Four
gates sit in front of K8s: **scope → resource allow-list → QPS → one-shot approval**.

```bash
# 1) Web UI → "MCP 与审批" → create a token (ns=flink, scopes=read,operate)
# 2) plug it into any MCP client
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

High-risk operations (suspend / scale / restore / image upgrade / delete) **cannot go
through** by default: the first call returns `approval_required:<id>`, a human approves in
the web UI, the client retries with `approvalToken` — valid exactly once, within 300s.

HTTP transport: `flinkops-mcp --http 127.0.0.1:8090` (`POST /mcp`).

---

## Status

| Area | Status |
|---|---|
| Backend layering (9 modules) | ✅ `main.rs` is wiring only; `routes/jobs.rs` is the largest file (22 job endpoints in one domain module, not yet split) |
| 22 job endpoints (list/detail/metrics/log/start-stop/restart/savepoint/restore/scale/upgrade/ingress/autoscaler/backpressure/flamegraph) | ✅ verified on a real cluster |
| Read-only reverse proxy (query inspection, WebSocket tunneling) | ✅ writes rejected with 403; `/ws` handshake passes |
| Editor support `validate` / `diff` / `yaml` / `render` | ✅ same rules as submission |
| Events / exceptions / snapshots | ✅ |
| Audit (retention + size checks) | ✅ |
| Shared store | ✅ SQLite (PVC / single instance) · PostgreSQL · MySQL (shared metadata across replicas) |
| Auth | ✅ local account; sessions in the shared store (survive restarts) |
| Job submission | ✅ `POST /api/jobs` with `?dryRun=true` preflight |
| MCP | ✅ 17 tools + token/scope/QPS/approval, see [`docs/mcp-spec.md`](docs/mcp-spec.md) |
| WebSocket proxy | ⚠️ Flink UI degrades to polling in some paths |

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd apps/web && npm run build          # vue-tsc + vite
cargo deny check licenses             # allow-list in deny.toml
```

## Limitations

- **Dynamically added clusters** persist in the store, but env-defined clusters win on id clash.
- **Session clusters** need `mode: native` and **no `job` block** (the old `mode=session`
  enum was removed from the operator).
- **Template generation** drops the whole `podTemplate` on purpose (it often carries AK/SK),
  so generated templates contain no initContainers.
- **Single account** (`FLINKOPS_ADMIN_USER/PASSWORD`); no multi-tenancy.
- **Checkpoint metrics from JobManager** require a PodMonitor that selects both
  `component=jobmanager` and `component=taskmanager`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) ([简体中文](CONTRIBUTING.zh-CN.md)) and
[SECURITY.md](SECURITY.md) for vulnerability reports.

## License

Apache-2.0 (see [LICENSE](LICENSE) and [NOTICE](NOTICE)).
Distributed artifacts contain no JVM, no Flink distribution and no GPL connectors.

"Apache Flink", "Flink" and the Flink logo are trademarks of the Apache Software
Foundation. This project is not affiliated with, endorsed by, or sponsored by the ASF.
