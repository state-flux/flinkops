# 数据源规则

[English](data-sources.en.md) | **简体中文**

> 这是本项目最容易写错的地方。新增任何"取数"逻辑前先读这一页。

## 1. 哪类数据只能从哪来

| 数据 | 来源 | 理由 |
|---|---|---|
| 作业列表 / 状态 / 条件 | **CRD**（本地索引） | 一次 list 拿全部；实时性由 reconcile 周期决定（≤30s） |
| 实时指标 / 背压 / 水位线 | **Flink REST**（`?get=` 批量） | 毫秒级；Prometheus 有 15–60s 延迟 |
| 历史 / 跨作业聚合 | Prometheus（可选） | 只有它能给历史；不配置就只有实时 |
| checkpoint 计数 | REST `/jobs/:id/checkpoints` | CRD 里只有 `lastCheckpoint` |
| 日志 | **Pod logs 优先**；REST 文件日志需 log4j 配 appender | K8s 默认只有 stdout |
| 谁在跑（真相） | `status.conditions[Running=True]`；老 operator 用 `lifecycleState=STABLE` | 比 Pod Ready 可信 |

**禁止**：为了让曲线好看去高频轮询 `/backpressure`（每次采样 100 次任务线程，30–60s 一次即可）。

## 2. JobManager REST 的地址怎么来（易错点）

平台可能跑在两个位置，选路必须分开：

| 平台位置 | REST 地址 | 说明 |
|---|---|---|
| 集群内（Pod） | `http://{name}-rest.{ns}.svc.cluster.local:8081` | 最快，且不占节点端口 |
| 集群外（kubeconfig） | `http://<api-server-host>:<nodePort>` | 读 Service `{name}-rest` 的 `nodePort`，host 取 kubeconfig 的 server |
| 集群外 + Service 是 ClusterIP | 无法直连 | 明确报错：请把 `kubernetes.rest-service.exposed.type` 设为 NodePort，或 port-forward |

实现：`k8s::Cluster::rest_endpoint`；结果按 `cluster/ns/name` 缓存 60s（nodePort 不常变）。

**踩过的坑**：早期版本一律用集群内 DNS，导致在 Windows 本机跑平台时所有实时指标都判成"不可用"——而作业明明是 NodePort 暴露的。

## 3. 指标层级（Flink 1.15 实测）

| 层级 | 端点 | 有什么 |
|---|---|---|
| Job | `/jobs/:id/metrics` | `numberOfCompletedCheckpoints`、`numberOfFailedCheckpoints`、`lastCheckpointDuration/Size`、`fullRestarts`、`uptime` |
| Vertex | `/jobs/:id/vertices/:vid/metrics` | `numRecordsIn/Out`（吞吐）、`busyTimeMsPerSecond`、背压相关 |
| JobManager | `/jobmanager/metrics` | `numRegisteredTaskManagers`、`taskSlotsTotal` |

两个必须知道的差异：

1. **响应格式**：Flink ≤1.15 返回数组 `[{"id":"x","value":"1"}]`，新版本返回 `{"id":"jobid","metrics":[…]}`。解析必须兼容两种（`parse_metrics`）。
2. **Job 级没有吞吐**：`numRecordsInPerSecond` 在 job 级不存在，必须按 vertex 求和（`job_throughput`），再用两次采样差算速率（`throughput_cache`）。

## 4. 降级语义

- 实时源失败 → 返回 `source:"none"|"prometheus"` + `degraded:"<原因>"`，`values` 为空；
- 前端**必须显式提示**"实时源不可用"，不能把空值当 0；
- 详情页其余数据仍来自 CRD，所以降级不影响主流程。

## 5. 存储边界

只持久化：作业索引、审计、环境基线、会话。**不存指标点**。
保留期：审计 90 天（`FLINKOPS_AUDIT_RETENTION_DAYS`）+ 每小时清理；会话按过期时间清理。
## 6. WebSocket 反代

Flink Web UI 的实时推送走 `/ws`。平台的做法：

- 手工调用 `WebSocketUpgrade::from_request_parts`（`Request` 只能消费一次，先拆 parts+body，不是 WS 再拼回去）；
- 下游用 axum 的 `WebSocket`，上游用 `tokio-tungstenite::connect_async("ws://<rest-endpoint>/ws")`；
- 双向按消息转发（Text/Binary/Ping/Pong），任一侧关闭即整体结束；
- 上游地址与 REST 同一套选路（集群内 DNS / 集群外 NodePort），所以 WS 和 HTTP 永远打到同一个 JM。

验证：`curl -i -N` 带 `Sec-WebSocket-Key` 应返回 `101` 且 `sec-websocket-accept` 正确；审计里会出现 `ui.open` + `ws ws://<node-ip>:<nodePort>/ws`。

## 7. Prometheus 接入

平台**不配置也能跑**，但历史曲线必须有 Prometheus。

地址解析顺序：

1. `FLINKOPS_PROMETHEUS_URL`（显式，推荐生产）
2. 自动发现：列集群 Service，名字含 `prometheus` 且不是 `alertmanager/operator/exporter` 的；平台在集群外用其 `nodePort` + api-server host 拼 URL
3. 都没有 → 返回 404 并提示怎么配

结果缓存 5 分钟。诊断接口：`GET /api/prometheus/discover`。

### 有 Prometheus ≠ 有 Flink 指标

实测（Rancher monitoring）：`up` 有 195 个 target，但 `count by (__name__)({__name__=~"flink_.+"})` 为 0 —— 说明 Flink 容器 9250 端口没被抓取。

要抓取，只需要 PodMonitor（**不需要改作业**，Operator 自带 `component=taskmanager` / `component=jobmanager` 标签）。
**两个组件都要抓**：只抓 TM 会丢掉整块 `flink_jobmanager_*`（checkpoint 时长/次数全在 JM 侧）：

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: flink-taskmanagers
  namespace: flink
  labels:
    release: rancher-monitoring   # 必须匹配 Prometheus CR 的 podMonitorSelector
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

> 别用一个 PodMonitor 同时选两个组件，除非集群里**没有**旧的 taskmanager-only PodMonitor：
> 两个 PM 同时命中同一个 Pod，Prometheus 会存两份样本（`job` 标签不同），`sum()` 直接翻倍。

apply 之后 30–60s，`flink_*` 指标就会出现，`/metrics` 页的历史曲线立刻有数据。