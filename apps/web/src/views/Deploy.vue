<script setup lang="ts">
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import { auth, system } from "../api/client";
import { download, notify } from "../lib/notify";
import { t } from "../lib/i18n";

const tab = ref("d-docker");
const me = useQuery({ queryKey: ["me"], queryFn: auth.me });
const info = useQuery({ queryKey: ["cluster-info"], queryFn: system.clusterInfo, retry: false });
const clusters = computed(() => me.data?.value?.clusters ?? []);

// 接入自检：5 步，用真实接口逐项判定（不是文档式的"应该"）
const checks = computed(() => {
  const i = info.data?.value;
  const c = i?.clusters?.[0];
  return [
    {
      label: t("dep.check.controlPlane"),
      ok: !!me.data?.value,
      detail: me.data?.value
        ? t("dep.check.loggedIn", { user: me.data.value.user })
        : t("dep.check.notLoggedIn"),
    },
    {
      label: t("dep.check.clusterOk"),
      ok: !!c,
      detail: c
        ? `${c.id} (${c.inCluster ? "in-cluster" : "kubeconfig"})`
        : t("dep.check.noCluster"),
    },
    {
      label: t("dep.check.crds"),
      ok: !!c?.supportedKinds?.includes("FlinkDeployment"),
      detail: (c?.supportedKinds ?? []).join(" / ") || "—",
    },
    {
      label: t("dep.check.operator"),
      ok: (c?.operators?.[0]?.readyReplicas ?? 0) > 0,
      detail: c?.operators?.[0]
        ? `${c.operators[0].namespace} · ready ${c.operators[0].readyReplicas}`
        : t("dep.check.operatorMissing"),
    },
    {
      label: t("dep.check.index"),
      ok: (i?.jobs?.total ?? 0) > 0,
      detail: t("dep.check.indexDetail", {
        n: i?.jobs?.total ?? 0,
        withC: i?.jobs?.withConditions ?? 0,
      }),
    },
    {
      label: t("dep.check.metrics"),
      ok: !!me.data?.value?.prometheus,
      detail: me.data?.value?.prometheus ? t("dep.check.promOn") : t("dep.check.promOff"),
    },
  ];
});
const checkOk = computed(() => checks.value.filter((c) => c.ok).length);

const valuesYaml = `store:
  type: sqlite
  size: 1Gi
  storageClass: ""

metrics:
  backend: none
  prometheusUrl: ""

clusters: "local=incluster"
readonly: false

ingress:
  enabled: false
  className: nginx
  host: flinkops.example.com
`;

const pageActions = computed(() => [
  {
    label: t("dep.downloadValues"),
    run: () => {
      download("values.yaml", valuesYaml, "text/yaml;charset=utf-8");
      notify(t("dep.downloaded"), "ok");
    },
  },
  {
    label: t("dep.connectCluster"),
    // 接入动作在「集群」页（路径 / 粘贴 kubeconfig + 连通性测试），这里直接跳过去
    run: () => window.location.assign("/clusters"),
  },
  {
    label: t("dep.probe"),
    run: async () => {
      try {
        const info = await system.clusterInfo();
        const kinds = info.clusters.reduce((n, c) => n + c.kindCount, 0);
        notify(
          t("dep.probeOk", {
            clusters: info.clusters.length,
            kinds,
            jobs: info.jobs.total,
          }),
          "ok",
        );
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
]);

const cmds: Record<string, string> = {
  "d-docker": `# 1) 构建（前端在镜像里构建，不需要本地 Node）
docker build -f deploy/Dockerfile -t flinkops:0.1.0 .

# 2) 运行：挂 kubeconfig + 持久化 SQLite
docker run -d --name flinkops -p 8080:8080 \\
  -v flinkops-data:/data \\
  -v ~/.kube/config:/etc/flinkops/kubeconfig:ro \\
  -e FLINKOPS_CLUSTERS="prod=/etc/flinkops/kubeconfig" \\
  -e FLINKOPS_ADMIN_PASSWORD='change-me' \\
  flinkops:0.1.0`,
  "d-k8s": `helm install flinkops deploy/helm/flinkops -n flinkops --create-namespace \\
  --set store.type=sqlite --set store.size=1Gi \\
  --set auth.existingSecret=flinkops-auth \\
  --set ingress.enabled=true --set ingress.className=nginx \\
  --set ingress.host=flinkops.example.com

# 多副本 / 跨集群：改用 postgres，并给每个集群挂 kubeconfig
--set store.type=postgres --set store.existingSecret=flinkops-db`,
  "d-k3s": `# k3s 四个必须知道的差异
# 1. IngressClass 是 traefik（不是 nginx）
helm install flinkops deploy/helm/flinkops -n flinkops --create-namespace \\
  --set ingress.enabled=true --set ingress.className=traefik

# 2. StorageClass 用 local-path（单节点）
--set store.storageClass=local-path

# 3. 集群内 DNS 是 *.svc.cluster.local（平台据此直连 JobManager）
--set clusters="local=incluster"

# 4. k3s 的 kubeconfig server 常写成 127.0.0.1:6443
#    要跨机使用时，改成 https://<节点IP>:6443`,
  "d-join": `# 接入一个已有的 flink-kubernetes-operator
# 1) 确认 CRD 存在
kubectl get crd | grep flink.apache.org

# 2) 平台需要的最小权限（只读 + 写 4 个 CRD）
kubectl apply -f deploy/helm/flinkops/templates/flinkops.yaml

# 3) 告诉平台连哪个集群
FLINKOPS_CLUSTERS="prod=/etc/flinkops/kubeconfig,dev=incluster"

# 4) 只读模式（不想让人误停生产）
FLINKOPS_READONLY=true`,
  "d-check": `# 自检（会用默认基线生成 spec 并校验）
flinkopsctl doctor

# 期望输出
#   [1] 基线 general-1.19 → 生成 spec：0 条 finding（0 error）
#   [2] 静态目录 ./apps/web/dist: ok
#   [3] 控制面 http://127.0.0.1:8080: ok
#   [4] 登录成功：user=admin clusters=1 prometheus=false`,
};
</script>

<template>
  <PageHead
    :title="t('page.deploy')"
    :sub="t('dep.sub')"
    :badge="t('dep.badge', { n: clusters.length })"
    :tone="clusters.length ? 'ok' : 'warn'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'd-docker', label: 'Docker' },
      { key: 'd-k8s', label: 'Kubernetes' },
      { key: 'd-k3s', label: 'k3s' },
      { key: 'd-join', label: t('dep.tab.operator') },
      { key: 'd-check', label: t('dep.tab.selfCheck') },
    ]"
  />

  <Card :title="tab === 'd-k3s' ? 'k3s 差异' : '命令'" :hint="t('dep.valuesHint')">
    <pre
      class="overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed text-fg-1"
      >{{ cmds[tab] }}</pre>
  </Card>

  <div class="mt-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(260px, 1fr))">
    <Card :title="t('dep.selfCheck')" :hint="`${checkOk}/${checks.length} 通过`" :pad="false">
      <div class="divide-y divide-line-2/70">
        <div v-for="(c, i) in checks" :key="i" class="flex items-start gap-2 px-4 py-2">
          <span
            class="mt-0.5 grid h-4 w-4 shrink-0 place-items-center rounded-full text-3xs"
            :class="c.ok ? 'bg-ok/15 text-ok' : 'bg-warn/15 text-warn'"
            >{{ c.ok ? "✓" : "!" }}</span
          >
          <div class="min-w-0">
            <div class="text-xs">{{ c.label }}</div>
            <div class="truncate text-3xs text-fg-2" :title="c.detail">{{ c.detail }}</div>
          </div>
        </div>
      </div>
      <div class="border-t border-line-2 px-4 py-2 text-3xs text-fg-2">
        {{ t("dep.selfCheckNote") }}
      </div>
    </Card>

    <Card :title="t('dep.objectsCard')" :pad="false">
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">{{ t("dep.col.object") }}</th>
              <th class="px-3 py-2">{{ t("dep.col.note") }}</th>
              <th class="px-3 py-2">{{ t("dep.col.mode") }}</th>
              <th class="px-3 py-2">{{ t("dep.col.usage") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr class="border-b border-line-2/60">
              <td class="px-3 py-1.5 font-mono">Deployment</td>
              <td class="px-3 py-1.5 text-fg-2">{{ t("dep.obj.binary") }}</td>
              <td class="px-3 py-1.5">helm</td>
              <td class="px-3 py-1.5">{{ t("dep.production") }}</td>
            </tr>
            <tr class="border-b border-line-2/60">
              <td class="px-3 py-1.5 font-mono">ConfigMap/Secret</td>
              <td class="px-3 py-1.5 text-fg-2">{{ t("dep.obj.auth") }}</td>
              <td class="px-3 py-1.5">helm values</td>
              <td class="px-3 py-1.5">{{ t("dep.general") }}</td>
            </tr>
            <tr class="border-b border-line-2/60">
              <td class="px-3 py-1.5 font-mono">PVC</td>
              <td class="px-3 py-1.5 text-fg-2">{{ t("dep.obj.meta") }}</td>
              <td class="px-3 py-1.5">helm storageClass</td>
              <td class="px-3 py-1.5">{{ t("dep.singleReplica") }}</td>
            </tr>
            <tr class="border-b border-line-2/60">
              <td class="px-3 py-1.5 font-mono">Ingress</td>
              <td class="px-3 py-1.5 text-fg-2">{{ t("dep.obj.console") }}</td>
              <td class="px-3 py-1.5">nginx / traefik</td>
              <td class="px-3 py-1.5">{{ t("dep.external") }}</td>
            </tr>
            <tr>
              <td class="px-3 py-1.5 font-mono">ClusterRole</td>
              <td class="px-3 py-1.5 text-fg-2">{{ t("dep.obj.rbac") }}</td>
              <td class="px-3 py-1.5">helm rbac</td>
              <td class="px-3 py-1.5">{{ t("dep.required") }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>
  </div>

  <div class="mt-3 grid gap-3" style="grid-template-columns: repeat(auto-fit, minmax(260px, 1fr))">
    <Card :title="t('dep.statusCard')">
      <div class="flex flex-wrap gap-2">
        <Badge v-for="c in clusters" :key="c" tone="ok" dot>{{ c }}</Badge>
        <span v-if="!clusters.length" class="text-xs text-warn">{{ t("dep.noCluster") }}</span>
      </div>
      <p class="mt-2 text-2xs text-fg-2">
        {{ t("dep.sourcePrefix") }}<code class="font-mono">FLINKOPS_CLUSTERS</code
        >{{ t("dep.sourceFallback") }}<code class="font-mono">.secrets/kubeconfig.yaml</code>。
      </p>
    </Card>
    <Card :title="t('dep.whyCard')">
      <p class="text-2xs leading-relaxed text-fg-2">{{ t("dep.whyBody") }}</p>
    </Card>
  </div>
</template>
