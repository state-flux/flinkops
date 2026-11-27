//! 运行时访问：REST 端点、NodePort、Pod 日志、事件、快照、watch。

#![allow(unused_imports)]
use super::*;

impl Cluster {
    /// 解析某个作业的 JobManager REST 地址。
    ///
    /// 回退链（这是踩过的坑）：
    /// 1. 平台在集群内 → `{name}-rest.{ns}.svc.cluster.local:8081`，最快且不占节点端口；
    /// 2. 平台在集群外 → 读 Service `{name}-rest` 的 nodePort，用 api-server 的 host 拼；
    /// 3. Service 是 ClusterIP（无 nodePort）→ 明确报错，而不是拿一个必然失败的 DNS 去打。
    pub async fn rest_endpoint(&self, ns: &str, name: &str) -> Result<RestEndpoint> {
        use k8s_openapi::api::core::v1::Service;

        if self.in_cluster {
            return Ok(RestEndpoint::Dns(format!(
                "http://{name}-rest.{ns}.svc.cluster.local:8081"
            )));
        }

        let svc_name = format!("{name}-rest");
        let api: Api<Service> = Api::namespaced(self.client.clone(), ns);
        let svc = api.get(&svc_name).await.map_err(|e| Error::Upstream {
            target: format!("service {ns}/{svc_name}"),
            message: format!("{e}（Operator 可能没创建 rest Service）"),
        })?;

        let spec = svc.spec.clone().unwrap_or_default();
        let ports = spec.ports.clone().unwrap_or_default();
        // 8081 是 JM REST；老版本 operator 的 port 名可能是 rest
        let port = ports
            .iter()
            .find(|p| p.port == 8081 || p.name.as_deref() == Some("rest"))
            .or_else(|| ports.first());

        let node_port = port.and_then(|p| p.node_port);
        let svc_type = spec.type_.clone().unwrap_or_else(|| "ClusterIP".into());

        match (node_port, self.api_host.clone()) {
            (Some(np), Some(host)) => {
                if host == "127.0.0.1" || host == "localhost" || host == "::1" {
                    return Err(Error::Upstream {
                        target: format!("{ns}/{name} REST"),
                        message: format!(
                            "NodePort {np} 需要用节点 IP 访问，但 kubeconfig 里 server 是 {host}；\
                             请把 server 改成 https://<节点IP>:6443"
                        ),
                    });
                }
                Ok(RestEndpoint::NodePort { host, port: np })
            }
            (Some(np), None) => Err(Error::Upstream {
                target: format!("{ns}/{name} REST"),
                message: format!("Service 是 {svc_type}，nodePort={np}，但无法确定节点地址"),
            }),
            (None, _) => Err(Error::Upstream {
                target: format!("{ns}/{name} REST"),
                message: format!(
                    "Service {svc_name} 是 {svc_type}（无 nodePort），平台在集群外无法直连；\
                     请把 kubernetes.rest-service.exposed.type 设为 NodePort，或用 port-forward"
                ),
            }),
        }
    }

    /// 读某个 Service 的 nodePort（平台在集群外访问 SQL Gateway 用）。
    pub async fn service_node_port(&self, ns: &str, name: &str) -> Result<Option<i32>> {
        use k8s_openapi::api::core::v1::Service;
        let api: Api<Service> = Api::namespaced(self.client.clone(), ns);
        match api.get_opt(name).await {
            Ok(Some(s)) => Ok(s
                .spec
                .and_then(|sp| sp.ports)
                .and_then(|p| p.first().and_then(|x| x.node_port))),
            Ok(None) => Ok(None),
            Err(e) => Err(map_err("service", e)),
        }
    }

    /// 读 Pod 日志（JobManager / TaskManager）。
    ///
    /// K8s 默认只把容器 stdout 落到节点上；Flink 的 `/jobmanager/log` 只有在
    /// log4j 配了 file appender 时才存在。REST 拿不到日志时这是唯一可用来源。
    pub async fn pod_logs(
        &self,
        ns: &str,
        selector: &str,
        tail_lines: i64,
    ) -> Result<(String, String)> {
        use k8s_openapi::api::core::v1::Pod;
        let api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
        let list = api
            .list(&ListParams::default().labels(selector).limit(1))
            .await
            .map_err(|e| map_err("pods", e))?;
        let pod = list
            .items
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("没有匹配 {selector} 的 Pod")))?;
        let name = pod.metadata.name.clone().unwrap_or_default();
        let container = pod
            .spec
            .as_ref()
            .and_then(|s| s.containers.first())
            .map(|c| c.name.clone());
        let params = LogParams {
            container,
            tail_lines: Some(tail_lines.clamp(1, 100_000)),
            ..Default::default()
        };
        let logs = api
            .logs(&name, &params)
            .await
            .map_err(|e| map_err("pod logs", e))?;
        Ok((name, logs))
    }

    // ---------------------------------------------------------- watch

    /// 后台 watch 一类对象，变化即回调（`Upsert` / `Delete`）。
    ///
    /// 轮询全量 list 的问题是"延迟无上界 + 每次都是全量"。watch 让索引贴近实时；
    /// 断流（apiserver 滚动、429）时退避 3s 重建，重建会先跑一次 Init 全量。
    pub fn spawn_watcher<F>(&self, kind: &'static str, f: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(WatchOp, Value) + Send + 'static,
    {
        let api = self.api(kind, None);
        let id = self.id.clone();
        tokio::spawn(async move {
            loop {
                let stream = kube::runtime::watcher::watcher(api.clone(), Default::default());
                tokio::pin!(stream);
                let mut ended = false;
                while let Some(ev) = stream.next().await {
                    match ev {
                        Ok(kube::runtime::watcher::Event::Apply(o))
                        | Ok(kube::runtime::watcher::Event::InitApply(o)) => {
                            if let Ok(v) = serde_json::to_value(&o) {
                                f(WatchOp::Upsert, v);
                            }
                        }
                        Ok(kube::runtime::watcher::Event::Delete(o)) => {
                            if let Ok(v) = serde_json::to_value(&o) {
                                f(WatchOp::Delete, v);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(cluster = %id, kind, error = %e, "watch 中断，3s 后重建");
                            ended = true;
                            break;
                        }
                    }
                }
                let _ = ended;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        })
    }

    // ---------------------------------------------------------- events

    /// 读 core/v1 Event（服务端 fieldSelector 过滤，避免拉全量）。
    pub async fn list_events(&self, ns: &str, kind: &str, name: &str) -> Result<Vec<EventView>> {
        use k8s_openapi::api::core::v1::Event;

        let api: Api<Event> = Api::namespaced(self.client.clone(), ns);
        let selector = format!("involvedObject.name={name},involvedObject.kind={kind}");
        let list = api
            .list(&ListParams::default().fields(&selector))
            .await
            .map_err(|e| map_err("events", e))?;

        let mut out: Vec<EventView> = list
            .items
            .into_iter()
            .map(|e| EventView {
                kind: e.type_.unwrap_or_default(),
                reason: e.reason.unwrap_or_default(),
                message: e.message.unwrap_or_default(),
                count: e.count.unwrap_or(0) as i64,
                first_timestamp: e.first_timestamp.map(fmt_time),
                last_timestamp: e.last_timestamp.map(fmt_time),
                source: e.source.and_then(|s| s.component),
            })
            .collect();
        // 最新事件在前
        out.sort_by(|a, b| b.last_timestamp.cmp(&a.last_timestamp));
        Ok(out)
    }

    /// 列出 FlinkStateSnapshot（保存点/检查点的 CR 视图）。
    pub async fn list_snapshots(&self, ns: Option<&str>) -> Result<Vec<Value>> {
        self.list_raw(crd::KIND_SNAPSHOT, ns).await
    }

    /// 触发 savepoint：创建 FlinkStateSnapshot（而不是直接打 REST，这样有 CR 可追溯）。
    pub async fn create_savepoint(
        &self,
        ns: &str,
        job_name: &str,
        path: Option<&str>,
    ) -> Result<String> {
        self.ensure_writable()?;
        // 名字必须唯一：秒级时间戳会让"同一秒内点两次"生成同名 CR，第二次被
        // SSA 静默覆盖（两个请求都成功、实际只有一个快照）。毫秒级仍可能撞，
        // 再加 4 位随机后缀。
        let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S%3f").to_string();
        let rand4 = &uuid::Uuid::new_v4().simple().to_string()[..4];
        let name = format!("{job_name}-sp-{stamp}-{rand4}");
        // webhook 要求 checkpoint / savepoint 恰好一个，且 savepoint 必须带 path；
        // jobReference 只接受 kind/name（写 namespace 会报 field not declared）。
        // **kind 不能省**：最新 operator 的 SnapshotValidator 用 kind 定位目标资源，
        // 缺省会直接 403「Target for snapshot ns/name was not found」。
        let final_path = match path {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                let obj = self.get_raw(crd::KIND_DEPLOYMENT, ns, job_name).await?;
                let dir = crd::s(
                    &obj,
                    &["spec", "flinkConfiguration", "state.savepoints.dir"],
                )
                .ok_or_else(|| {
                    Error::BadRequest(format!(
                        "{job_name} 没有配 state.savepoints.dir；请在请求里显式给出 path"
                    ))
                })?;
                let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S%3f");
                format!("{}/sp-{stamp}", dir.trim_end_matches('/'))
            }
        };
        let spec = serde_json::json!({
            "jobReference": { "kind": "FlinkDeployment", "name": job_name },
            "savepoint": { "path": final_path }
        });
        let obj = serde_json::json!({
            "apiVersion": format!("{}/{}", crd::GROUP, crd::VERSION),
            "kind": crd::KIND_SNAPSHOT,
            "metadata": { "name": name, "namespace": ns },
            "spec": spec
        });
        tracing::info!(spec = %spec, "creating FlinkStateSnapshot");
        self.apply_custom(crd::GROUP, crd::VERSION, crd::KIND_SNAPSHOT, ns, obj)
            .await?;
        Ok(name)
    }
}
