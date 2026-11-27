Thanks for the PR. A few things this project cares about more than average:

## Checklist

- [ ] **Doc first**: if this changes an endpoint or behaviour, `docs/api-spec.md` (or
      `docs/mcp-spec.md`) is updated in the same PR.
- [ ] `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- [ ] `cd apps/web && npx vue-tsc -b && npm run build`
- [ ] New dependency? It is in the `deny.toml` allow-list (`cargo deny check licenses`).
- [ ] No credentials, kubeconfigs, tokens, or `last-applied-configuration` in the diff.
- [ ] If the UI shows a new number, it comes from the API — not from a hard-coded fallback.

## What and why

<!-- One paragraph: the operational problem, and why this change is the right shape. -->

## How it was verified

<!-- Real cluster? kind? Which job? Paste the request and response, or a screenshot. -->

## Risk

<!--
Write operations, RBAC changes, MCP scope changes, and anything touching the Flink UI proxy
need an explicit note here: what could go wrong on a production cluster, and how to roll back.
-->
