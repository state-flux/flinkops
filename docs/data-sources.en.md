# Data source rules

**English** | [简体中文](data-sources.md)

> This is the easiest place in the project to get wrong. Read this page before adding any
> code that fetches data.

## 1. Which data may only come from where

| Data | Source | Why |
|---|---|---|
| Job list / state / conditions | **CRD** (local index) | one list call; freshness bounded by the reconcile loop (≤30s) |
| Realtime metrics / backpressure / watermark | **Flink REST** (`?get=` batching) | millisecond level; Prometheus lags 15–60s |
| History / cross-job aggregation | Prometheus (optional) | only it has history; without it you get realtime only |
| Checkpoint counts | REST `/jobs/:id/checkpoints` | the CRD only stores `lastCheckpoint` |
| Logs | **pod logs first**; REST file logs need a log4j appender | K8s only keeps stdout by default |
| Who is really running | `status.conditions[Running=True]`; older operators use `lifecycleState=STABLE` | more trustworthy than pod readiness |

**Don't** poll `/backpressure` at high frequency to make charts look smooth — each sample
walks every task thread; once every 30–60s is enough.

## 2. Where the JobManager REST address comes from (easy to break)

The platform can run in two places; address resolution must differ:

| Platform location | REST address | Notes |
|---|---|---|
| Inside the cluster (pod) | `http://{name}-rest.{ns}.svc.cluster.local:8081` | fastest, no node ports consumed |
| Outside the cluster (kubeconfig) | `http://<api-server-host>:<nodePort>` | reads `nodePort` from Service `{name}-rest`; host comes from the kubeconfig server |
| Outside + Service is `ClusterIP` | not reachable | explicit error: set `kubernetes.rest-service.exposed.type` to `NodePort`, or port-forward |

Implementation: `k8s::Cluster::rest_endpoint`; results cached per `cluster/ns/name` for 60s
(node ports rarely change).

**A mistake we already made**: an early version always used the in-cluster DNS name, so when
the platform ran on a laptop every realtime metric was reported unavailable — while the jobs
were in fact exposed through NodePort.

## 3. Metric levels (measured on Flink 1.15)

| Level | Endpoint | What you get |
|---|---|---|
| Job | `/jobs/:id/metrics` | `numberOfCompletedCheckpoints`, `lastCheckpointDuration/Size`, `fullRestarts`, `uptime` |
| Vertex | `/jobs/:id/vertices/:vid/metrics` | `numRecordsIn/Out` (throughput), `busyTimeMsPerSecond`, backpressure |
| JobManager | `/jobmanager/metrics` | `numRegisteredTaskManagers`, `taskSlotsTotal` |

Two differences you must know:

1. **Response shape**: Flink ≤1.15 returns an array `[{"id":"x","value":"1"}]`; newer versions
   return `{"id":"jobid","metrics":[…]}`. Parsing must accept both (`parse_metrics`).
2. **No throughput at job level**: `numRecordsInPerSecond` does not exist there; sum it over
   vertices (`job_throughput`) and derive the rate from two samples (`throughput_cache`).

## 4. Degradation semantics

- Realtime source failure → return `source:"none"|"prometheus"` plus `degraded:"<reason>"`,
  with empty `values`;
- the frontend **must** say "realtime source unavailable" — never render an empty value as 0;
- the rest of the detail page still comes from the CRD, so degradation does not block the flow.

## 5. Storage boundary

Only persisted: job index, audit, environment baseline, sessions. **No metric points.**
Retention: audit 90 days (`FLINKOPS_AUDIT_RETENTION_DAYS`) with hourly cleanup; sessions expire
on their own timestamp.

## 6. WebSocket proxy

The Flink Web UI pushes realtime updates over `/ws`. How the platform does it:

- calls `WebSocketUpgrade::from_request_parts` manually (`Request` can only be consumed once:
  split parts+body first, reassemble when it is not a WS request);
- downstream is axum's `WebSocket`, upstream is
  `tokio-tungstenite::connect_async("ws://<rest-endpoint>/ws")`;
- messages are forwarded both ways (Text/Binary/Ping/Pong); closing either side ends both;
- the upstream address uses the same resolution as REST, so WS and HTTP always hit the same JM.

Verify: `curl -i -N` with a `Sec-WebSocket-Key` should return `101` and a correct
`sec-websocket-accept`; the audit log shows `ui.open` plus `ws ws://<node-ip>:<nodePort>/ws`.

## 7. Prometheus onboarding

The platform works **without** Prometheus, but history needs it.

Address resolution order:

1. `FLINKOPS_PROMETHEUS_URL` (explicit; recommended in production)
2. auto-discovery: list cluster Services, pick names containing `prometheus` that are not
   `alertmanager/operator/exporter`; from outside the cluster combine their `nodePort` with the
   API server host
3. neither → 404 explaining how to configure it

Result cached for 5 minutes. Diagnostics: `GET /api/prometheus/discover`.

### Prometheus ≠ Flink metrics

Measured on Rancher monitoring: `up` had 195 targets but
`count by (__name__)({__name__=~"flink_.+"})` returned 0 — nothing scraped port 9250.

You only need PodMonitors (**no job changes**; the operator already labels pods with
`component=taskmanager` / `component=jobmanager`). **Scrape both components**: taskmanager only
drops the whole `flink_jobmanager_*` family (checkpoint duration/counts live on the JM side):

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-taskmanagers
  namespace: flink
  labels:
    release: rancher-monitoring   # must match the Prometheus CR podMonitorSelector
spec:
  selector:
    matchLabels: { component: taskmanager }
  podMetricsEndpoints:
    - { port: prom-9250, path: /metrics, interval: 30s }
  namespaceSelector:
    matchNames: ["flink"]
---
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-jobmanagers
  namespace: flink
  labels:
    release: rancher-monitoring
spec:
  selector:
    matchLabels: { component: jobmanager }
  podMetricsEndpoints:
    - { port: prom-9250, path: /metrics, interval: 30s }
  namespaceSelector:
    matchNames: ["flink"]
```

> Do not use one PodMonitor for both components unless there is **no** older taskmanager-only
> PodMonitor in the cluster: two monitors matching the same pod store the samples twice (different
> `job` labels) and every `sum()` doubles.

30–60s after applying, `flink_*` appears and the history charts on `/metrics` fill in.
