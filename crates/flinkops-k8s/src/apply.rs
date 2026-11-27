//! 声明式下发：ConfigMap / SQL Gateway / Ingress 的创建与更新。

#![allow(unused_imports)]
use super::*;

impl Cluster {
    /// 给 session cluster 配一个 SQL Gateway（独立 Deployment + Service）。
    ///
    /// Flink 的 SQL Gateway 不是 FlinkDeployment 的一部分，是另一个进程；
    /// 它靠 FLINK_PROPERTIES 的 jobmanager.rpc.address 连到 session cluster。
    pub async fn apply_sql_gateway(
        &self,
        ns: &str,
        cluster_name: &str,
        image: &str,
        service_account: &str,
        node_port: Option<i32>,
    ) -> Result<(String, Option<i32>)> {
        use k8s_openapi::api::apps::v1::Deployment;
        use k8s_openapi::api::core::v1::Service;

        self.ensure_writable()?;
        let gw = format!("{cluster_name}-sql-gateway");
        let labels = serde_json::json!({ "app": gw, "flinkops.io/managed-by": "flinkops" });

        // 定制镜像常把 sql-gateway.sh 删掉/挪走：多路径探测，找不到就把 bin 目录打出来
        // 坑：仅靠 FLINK_PROPERTIES 的 rest.address 会被 sql-gateway 自己的
        // endpoint.rest.address(0.0.0.0) 覆盖 → 它去连 0.0.0.0:8081。
        // 所以集群连接参数必须显式走 -D。
        let script = format!(
            "for p in /opt/flink/bin/sql-gateway.sh /flink/bin/sql-gateway.sh /opt/flink/bin/sql-gateway; do \
if [ -x \"$p\" ]; then echo \"[flinkops] using $p -> {jm}\"; \
exec \"$p\" start-foreground \
-Drest.address={jm} -Drest.port=8081 \
-Djobmanager.rpc.address={jm} \
-Dexecution.target=remote \
-Dstate.checkpoints.dir=file:///tmp/flink-checkpoints \
-Dstate.savepoints.dir=file:///tmp/flink-savepoints \
-Dsql-gateway.endpoint.rest.address=0.0.0.0 \
-Dsql-gateway.endpoint.rest.port=8083; fi; done; \
echo '[flinkops] sql-gateway not found. FLINK_HOME contents:'; \
ls -1 /opt/flink/bin 2>/dev/null | head -40; echo '[flinkops] sleeping'; sleep 3600",
            jm = format!("{cluster_name}-rest")
        );

        let deploy = serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": { "name": gw, "namespace": ns, "labels": labels },
            "spec": {
                "replicas": 1,
                "selector": { "matchLabels": { "app": gw } },
                "template": {
                    "metadata": { "labels": labels },
                    "spec": {
                        "serviceAccount": service_account,
                        "containers": [{
                            "name": "sql-gateway",
                            "image": image,
                            "command": ["bash", "-c", script],
                            "ports": [{ "containerPort": 8083, "name": "sql-gateway" }],
                            "env": [{
                                // 1.16 的 SQL Gateway 用 execution.target=remote + rest.address 连集群；
                                // jobmanager.rpc.address 也要给（TM 连 JM 用），两个都写更稳。
                                "name": "FLINK_PROPERTIES",
                                "value": format!(
                                    "execution.target: remote\nrest.address: {cluster_name}-rest\nrest.port: 8081\njobmanager.rpc.address: {cluster_name}-rest\n"
                                )
                            }],
                            "resources": {
                                "requests": { "cpu": "200m", "memory": "512Mi" },
                                "limits": { "memory": "1Gi" }
                            }
                        }]
                    }
                }
            }
        });
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), ns);
        let obj: Deployment = serde_json::from_value(deploy)
            .map_err(|e| Error::BadRequest(format!("invalid deployment: {e}")))?;
        api.patch(
            &gw,
            &PatchParams::apply("flinkops").force(),
            &Patch::Apply(&obj),
        )
        .await
        .map_err(|e| map_err("sql-gateway", e))?;

        let svc = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": { "name": gw, "namespace": ns, "labels": labels },
            "spec": {
                "type": if node_port.is_some() { "NodePort" } else { "ClusterIP" },
                "selector": { "app": gw },
                "ports": [{
                    "name": "sql-gateway",
                    "port": 8083,
                    "targetPort": 8083,
                    "nodePort": node_port
                }]
            }
        });
        let svc_api: Api<Service> = Api::namespaced(self.client.clone(), ns);
        let svc_obj: Service = serde_json::from_value(svc)
            .map_err(|e| Error::BadRequest(format!("invalid service: {e}")))?;
        svc_api
            .patch(
                &gw,
                &PatchParams::apply("flinkops").force(),
                &Patch::Apply(&svc_obj),
            )
            .await
            .map_err(|e| map_err("sql-gateway-svc", e))?;

        // 回读实际分配的 nodePort（server 可能自己挑）
        let got = svc_api.get_opt(&gw).await.ok().flatten();
        let assigned = got
            .and_then(|s| s.spec)
            .and_then(|s| s.ports)
            .and_then(|p| p.first().and_then(|x| x.node_port));

        Ok((gw, assigned.or(node_port)))
    }

    /// 创建/更新 ConfigMap（SQL 脚本挂载用）。
    pub async fn apply_configmap(
        &self,
        ns: &str,
        name: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        use k8s_openapi::api::core::v1::ConfigMap;
        self.ensure_writable()?;
        let mut cm = ConfigMap::default();
        cm.metadata.name = Some(name.to_string());
        cm.metadata.namespace = Some(ns.to_string());
        cm.data = Some(data);
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        api.patch(
            name,
            &PatchParams::apply("flinkops").force(),
            &Patch::Apply(&cm),
        )
        .await
        .map_err(|e| map_err("configmap", e))?;
        Ok(())
    }

    /// 创建/更新 Ingress（networking.k8s.io/v1，server-side apply）。
    pub async fn apply_ingress(
        &self,
        ns: &str,
        name: &str,
        host: &str,
        service: &str,
        port: i32,
        class: &str,
    ) -> Result<()> {
        self.ensure_writable()?;
        let ar =
            ApiResource::from_gvk(&GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress"));
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), ns, &ar);
        let obj = serde_json::json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "Ingress",
            "metadata": { "name": name, "namespace": ns },
            "spec": {
                "ingressClassName": class,
                "rules": [{
                    "host": host,
                    "http": { "paths": [{
                        "path": "/",
                        "pathType": "Prefix",
                        "backend": { "service": { "name": service, "port": { "number": port } } }
                    }]}
                }]
            }
        });
        let dyn_obj: DynamicObject = serde_json::from_value(obj)
            .map_err(|e| Error::BadRequest(format!("invalid ingress: {e}")))?;
        // 不带 force：Ingress 名由调用方给（平台用 `{name}-rest-flinkops`，
        // 避免和 operator 根据 spec.ingress 生成的同名对象抢字段所有权）
        api.patch(
            name,
            &PatchParams::apply("flinkops"),
            &Patch::Apply(&dyn_obj),
        )
        .await
        .map_err(|e| map_err("ingress", e))?;
        Ok(())
    }
}
