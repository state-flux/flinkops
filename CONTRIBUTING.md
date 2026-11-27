# Contributing to FlinkOps

**English** | [简体中文](CONTRIBUTING.zh-CN.md)

Thanks for taking the time to contribute. This project talks to production Kubernetes
clusters, so the bar for "clearly correct" is higher than usual.

## Ground rules

1. **Doc first, code second.** Every endpoint or behaviour change starts as a diff in
   `docs/api-spec.md` (or `docs/mcp-spec.md`). If the doc and the code disagree, the doc is
   the bug report.
2. **No hidden state.** Anything the UI shows must come from the API; no hard-coded demo
   numbers, no fabricated fallbacks. When data is unavailable, render an honest empty state
   that says *why*.
3. **Writes go through the CRD.** No direct `kubectl`-style mutations from the UI layer.
   Lifecycle actions must land on `FlinkDeployment` / `FlinkStateSnapshot` and produce an
   audit row.
4. **One concern per PR.** Separate refactors from behaviour changes.

## Local setup

```bash
cargo build --workspace
# 前端统一用 npm（有 package-lock.json，CI 走 npm ci；不要混用 pnpm/yarn）
cd apps/web && npm install && npm run build && cd ../..
cp .env.example .env          # optional; defaults work in a dev container
cargo run -p flinkops-api --bin flinkops
```

Without a cluster you still get a working UI: point `FLINKOPS_CLUSTERS` at a kubeconfig, or
run against `kind` / `k3d` with the Flink Kubernetes Operator installed.

## Before you push

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/web && npx vue-tsc -b && npm run build
```

CI runs exactly these four commands. A PR that fails `clippy -D warnings` will not be
reviewed further.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/):

```
feat(mcp): add flinkops_scale tool with approval binding
fix(metrics): count only Source operators for throughput
docs(api): document /api/approvals/stream
```

Scope is the crate or area (`api`, `web`, `k8s`, `store`, `mcp`, `metrics`, `docs`).

## Adding an endpoint (five steps)

1. `docs/api-spec.md` — request, response, error codes, audit action.
2. `crates/flinkops-api/src/routes/<resource>.rs` — handler, `require_user`, then
   `mcp_guard::check_job_scope` / `gate_write` when the route touches a single job.
3. `crates/flinkops-api/src/routes/mod.rs` — register the route.
4. `apps/web/src/api/client.ts` — typed client function.
5. Unit test for anything with branching logic; a table row in the README status table if
   the feature is user-visible.

## Adding an MCP tool

Follow [`docs/mcp-spec.md`](docs/mcp-spec.md) §3. Read-only tools go in `tools.rs` with the
`flinkops_` prefix; anything that mutates a cluster needs a scope, a resource allow-list
check and an approval gate — the guard lives in the platform, not in the MCP binary.

## Code style

- Rust: `cargo fmt`, no `unwrap()` on paths reachable from a request.
- Vue: `<script setup lang="ts">`, no `any`, components under `components/`, one view per route.
- Comments: explain *why*, in the language of the surrounding file (Chinese and English are
  both fine; do not mix inside one file).

## Reporting bugs

Open an issue with: FlinkOps version, Flink / operator version, how the cluster is reached
(`incluster` or kubeconfig), the exact request, and the response. **Never paste tokens,
kubeconfigs, `last-applied-configuration`, or pod env values.**

For security issues see [SECURITY.md](SECURITY.md).
