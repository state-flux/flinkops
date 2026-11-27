# Security Policy

**English** | [简体中文](SECURITY.zh-CN.md)

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Use GitHub's private reporting: repository → **Security** → *Report a vulnerability*
(Security Advisories). If that is unavailable, contact the maintainers listed in
`CODEOWNERS` / the repository profile.

Include: affected version or commit, the configuration used (`FLINKOPS_CLUSTERS`,
whether the platform runs in-cluster), reproduction steps, and impact. We aim to
acknowledge within 3 business days.

## Threat model (what this project can do)

FlinkOps is an operations console, so a compromised instance is roughly equivalent to
`kubectl` against the clusters it is configured with. Concretely:

| Capability | Consequence if abused |
|---|---|
| Apply / patch / delete `FlinkDeployment`, `FlinkSessionJob`, `FlinkStateSnapshot` | stop, restart or destroy streaming jobs |
| Read pods, events and pod logs | read job data appearing in logs |
| Reverse-proxy the Flink Web UI | reach any JobManager REST endpoint (writes blocked at the proxy) |
| Query Prometheus | read any metric the Prometheus instance allows |

**There is no tenant isolation inside one instance.** Treat one deployment as one trust
domain; for several teams, run several instances with separate ServiceAccounts.

## Hardening checklist

- [ ] Change `FLINKOPS_ADMIN_PASSWORD`, or inject it from a Secret.
- [ ] Put the API behind TLS (`ingress.tls`); session cookies are `HttpOnly` and
      `SameSite=Lax`, and `Secure` requires TLS at the edge.
- [ ] Give the platform its own ServiceAccount with the narrowest RBAC that still works
      (`flinkdeployments`, `flinkstatesnapshots`, `services`, `pods`, `pods/log`, `events`).
- [ ] Run the platform **inside** the cluster and switch REST services to `ClusterIP`, so
      JobManagers are not reachable from office networks at all.
- [ ] Set `FLINKOPS_READONLY=true` where the console should be view-only.
- [ ] MCP tokens: minimum scopes, explicit `allowNamespaces` / `allowJobs`, short expiry.
      Tokens are stored as `sha256(secret)` and can be revoked at any time.
- [ ] Keep the audit table; it is the only record of who restarted what.

## What we do not consider a vulnerability

- The reverse proxy returning `502` for a JobManager that is unreachable from where the
  platform runs (`ClusterIP` from outside the cluster) — that is a deployment choice, and
  the UI says so.
- Metrics or logs that the configured Prometheus / Kubernetes API already exposes to the
  platform's ServiceAccount.
- Missing rate limiting on the browser session (MCP tokens do have a token bucket).

## Secrets in the repository

`.gitignore` excludes `.env*` (except `.env.example`), `*.db`, `.secrets/`, kubeconfigs and
keys. If you find a credential committed anyway, report it privately; we will rotate and
rewrite history.
