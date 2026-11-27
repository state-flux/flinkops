# 安全策略

[English](SECURITY.md) | **简体中文**

## 上报漏洞

安全问题上**请不要开公开 issue**。

用 GitHub 的私有通道：仓库 → **Security** → *Report a vulnerability*（Security Advisories）。
如果走不通，联系仓库资料里列出的维护者。

请带上：受影响的版本或 commit、使用的配置（`FLINKOPS_CLUSTERS`、平台是否在集群内）、
复现步骤、影响面。我们争取 3 个工作日内回复。

## 威胁模型（这个项目能做什么）

FlinkOps 是运维控制台，因此**一台被攻陷的实例 ≈ 对它所接的集群拥有 `kubectl` 能力**：

| 能力 | 被滥用后的后果 |
|---|---|
| Apply / patch / delete `FlinkDeployment`、`FlinkSessionJob`、`FlinkStateSnapshot` | 停掉、重启或删除流作业 |
| 读 Pod、事件、Pod 日志 | 看到日志里出现的业务数据 |
| 反代 Flink Web UI | 触达任意 JobManager REST 端点（写操作在代理层被拦） |
| 查询 Prometheus | 读到该 Prometheus 实例允许的任意指标 |

**一个实例内部没有租户隔离。** 一个部署 = 一个信任域；多团队请各起一套并用不同的
ServiceAccount。

## 加固清单

- [ ] 改掉 `FLINKOPS_ADMIN_PASSWORD`，或用 Secret 注入。
- [ ] API 放到 TLS 后面（`ingress.tls`）；会话 Cookie 是 `HttpOnly` + `SameSite=Lax`，
      `Secure` 需要入口处有 TLS。
- [ ] 给平台独立 ServiceAccount，RBAC 收到最窄（`flinkdeployments`、`flinkstatesnapshots`、
      `services`、`pods`、`pods/log`、`events`）。
- [ ] 平台部署**在集群内**，作业 REST 服务改 `ClusterIP`，让 JobManager 在办公网完全不可达。
- [ ] 只读场景把 `FLINKOPS_READONLY=true` 打开。
- [ ] MCP Token：最小 scope、明确写 `allowNamespaces` / `allowJobs`、短有效期。
      Token 只存 `sha256(secret)`，随时可吊销。
- [ ] 保留审计表——它是"谁在几点重启了什么"的唯一记录。

## 不算漏洞的情况

- 反代对"平台所在位置访问不到的 JobManager"返回 `502`（集群外用 `ClusterIP`）——
  这是部署选择，UI 里已经写明原因。
- 配置的 Prometheus / Kubernetes API 本来就会暴露给平台 ServiceAccount 的指标或日志。
- 浏览器会话没有限流（MCP Token 有令牌桶）。

## 仓库里的凭据

`.gitignore` 已排除 `.env*`（`.env.example` 除外）、`*.db`、`.secrets/`、kubeconfig 与
各类密钥。如果仍然发现了被提交的凭据，请走私密渠道；我们会轮换并重写历史。
