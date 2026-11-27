<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Card from "../components/Card.vue";
import DataTable from "../components/DataTable.vue";
import Modal from "../components/Modal.vue";
import { editor, jobs, templates, type TemplateItem } from "../api/client";
import { notify } from "../lib/notify";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

const tab = ref("tpl");
const genOpen = ref(false);
// 「导入 YAML 模板」：粘贴一份 manifest/spec，抽成模板 payload 存库
const importOpen = ref(false);
const importName = ref("");
const importYaml = ref("");
const importMsg = ref("");
const importBusy = ref(false);

function openImport() {
  importName.value = "";
  importYaml.value = "";
  importMsg.value = "";
  importOpen.value = true;
}

async function doImport() {
  if (!importName.value.trim()) {
    notify(t("tpl.needName"), "warn");
    return;
  }
  if (!importYaml.value.trim()) {
    notify(t("tpl.needYaml"), "warn");
    return;
  }
  importBusy.value = true;
  importMsg.value = "";
  try {
    // 先问服务端这份 YAML 合不合法（顺带把 spec 解析出来；只取 spec 部分入库）
    const v = await editor.validate(importYaml.value);
    const spec = (v.spec ?? {}) as Record<string, unknown>;
    if (!Object.keys(spec).length) {
      importMsg.value = t("tpl.noPayload");
      return;
    }
    await templates.create(importName.value.trim(), spec);
    notify(t("tpl.imported", { name: importName.value.trim() }), "ok");
    importOpen.value = false;
    await tpl.refetch();
  } catch (e) {
    importMsg.value = t("tpl.importFailed", { err: (e as Error).message });
  } finally {
    importBusy.value = false;
  }
}
const viewOpen = ref(false);
const editOpen = ref(false);
const editForm = ref({ id: "", name: "", payload: "" });
const picked = ref<TemplateItem | null>(null);
const form = ref({ job: "", name: "" });

/**
 * 连接器目录：只列 Flink 官方发行版提供的连接器，许可一栏是选用时最容易踩的坑
 * （GPL 连接器不能打进对外发行的镜像）。坐标可直接贴进 pom / 镜像 Dockerfile。
 */
const CONNECTORS = [
  {
    id: "kafka",
    name: "Kafka",
    dir: "flink-connector-kafka",
    license: "Apache-2.0",
    bundled: true,
    note: t("tpl.conn.kafkaNote"),
  },
  {
    id: "jdbc",
    name: "JDBC",
    dir: "flink-connector-jdbc",
    license: "Apache-2.0",
    bundled: true,
    note: t("tpl.conn.jdbcNote"),
  },
  {
    id: "filesystem",
    name: "FileSystem",
    dir: "flink-connector-files",
    license: "Apache-2.0",
    bundled: true,
    note: t("tpl.conn.fsNote"),
  },
  {
    id: "hdfs",
    name: "HDFS",
    dir: "flink-connector-hdfs",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.hdfsNote"),
  },
  {
    id: "elasticsearch",
    name: "Elasticsearch",
    dir: "flink-connector-elasticsearch",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.esNote"),
  },
  {
    id: "hbase",
    name: "HBase",
    dir: "flink-connector-hbase",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.hbaseNote"),
  },
  {
    id: "rabbitmq",
    name: "RabbitMQ",
    dir: "flink-connector-rabbitmq",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.rabbitNote"),
  },
  {
    id: "pulsar",
    name: "Pulsar",
    dir: "flink-connector-pulsar",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.pulsarNote"),
  },
  {
    id: "mongodb",
    name: "MongoDB",
    dir: "flink-connector-mongodb",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.mongoNote"),
  },
  {
    id: "kinesis",
    name: "Kinesis",
    dir: "flink-connector-kinesis",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.kinesisNote"),
  },
  {
    id: "dynamodb",
    name: "DynamoDB",
    dir: "flink-connector-dynamodb",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.dynamoNote"),
  },
  {
    id: "redis",
    name: "Redis",
    dir: "flink-connector-redis",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.redisNote"),
  },
  {
    id: "mysql-cdc",
    name: "MySQL CDC",
    dir: "flink-connector-mysql-cdc",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.mysqlCdcNote"),
  },
  {
    id: "postgres-cdc",
    name: "PostgreSQL CDC",
    dir: "flink-connector-postgres-cdc",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.pgCdcNote"),
  },
  {
    id: "mongo-cdc",
    name: "MongoDB CDC",
    dir: "flink-connector-mongodb-cdc",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.mongoCdcNote"),
  },
  {
    id: "maxwell",
    name: "Maxwell",
    dir: "flink-connector-maxwell",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.maxwellNote"),
  },
  {
    id: "hudi",
    name: "Hudi",
    dir: "hudi-flink",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.hudiNote"),
  },
  {
    id: "iceberg",
    name: "Iceberg",
    dir: "iceberg-flink-runtime",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.icebergNote"),
  },
  {
    id: "paimon",
    name: "Paimon",
    dir: "paimon-flink",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.paimonNote"),
  },
  {
    id: "starrocks",
    name: "StarRocks",
    dir: "flink-connector-starrocks",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.starrocksNote"),
  },
  {
    id: "doris",
    name: "Doris",
    dir: "flink-doris-connector",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.dorisNote"),
  },
  {
    id: "clickhouse",
    name: "ClickHouse",
    dir: "flink-connector-clickhouse",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.clickhouseNote"),
  },
  {
    id: "kudu",
    name: "Kudu",
    dir: "flink-connector-kudu",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.kuduNote"),
  },
  {
    id: "sqlserver-cdc",
    name: "SQL Server CDC",
    dir: "flink-connector-sqlserver-cdc",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.sqlserverCdcNote"),
  },
  {
    id: "oracle-cdc",
    name: "Oracle CDC",
    dir: "flink-connector-oracle-cdc",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.oracleCdcNote"),
  },
  {
    id: "protobuf",
    name: t("tpl.conn.formatName"),
    dir: "flink-format-protobuf",
    license: "Apache-2.0",
    bundled: false,
    note: t("tpl.conn.registryNote"),
  },
  {
    id: "habse",
    name: t("tpl.conn.gplName"),
    dir: "mysql-connector-java",
    license: "GPL-2.0",
    bundled: false,
    note: t("tpl.conn.gplNote"),
  },
];

const connKw = ref("");
const connList = computed(() => {
  const kw = connKw.value.trim().toLowerCase();
  if (!kw) return CONNECTORS;
  return CONNECTORS.filter(
    (c) => c.name.toLowerCase().includes(kw) || c.dir.toLowerCase().includes(kw),
  );
});

function copyCoord(c: { dir: string; name: string }) {
  const text = `<dependency>\n  <groupId>org.apache.flink</groupId>\n  <artifactId>${c.dir}</artifactId>\n  <version>\${flink.version}</version>\n</dependency>`;
  navigator.clipboard?.writeText(text).then(
    () => notify(t("tpl.coordsCopied", { name: c.name }), "ok"),
    () => notify(t("tpl.copyFailed"), "warn"),
  );
}

const tpl = useQuery({ queryKey: ["templates"], queryFn: templates.list });
const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list() });

const tplItems = computed(() => tpl.data?.value?.items ?? []);
const jobItems = computed(() => all.data?.value?.items ?? []);

const images = computed(() => {
  const m = new Map<string, { n: number; jobs: string[]; ver: Set<string> }>();
  for (const j of jobItems.value) {
    if (!j.image) continue;
    const e = m.get(j.image) ?? { n: 0, jobs: [], ver: new Set<string>() };
    e.n += 1;
    e.jobs.push(j.name);
    if (j.flinkVersion) e.ver.add(j.flinkVersion);
    m.set(j.image, e);
  }
  return [...m.entries()]
    .map(([img, e]) => ({
      img,
      n: e.n,
      jobs: e.jobs.slice(0, 3),
      tag: img.includes(":") ? (img.split(":").pop() ?? "") : "",
      mutable: !img.includes("@sha256:"),
      ver: [...e.ver].join(" / "),
    }))
    .sort((a, b) => b.n - a.n);
});
const mutableCount = computed(() => images.value.filter((i) => i.mutable).length);

const pageActions = computed(() => [
  {
    label: t("tpl.refresh"),
    run: async () => {
      await tpl.refetch();
      notify(t("tpl.refreshed"), "ok");
    },
  },
  {
    label: t("tpl.createFromJob"),
    run: () => {
      form.value = { job: "", name: "" };
      genOpen.value = true;
    },
    primary: true,
  },
  { label: t("tpl.importYaml"), run: () => openImport() },
]);

async function generate() {
  if (!form.value.job) {
    notify(t("tpl.pickSource"), "warn");
    return;
  }
  const parts = form.value.job.split("/");
  const cluster = parts[0] ?? "";
  const ns = parts[1] ?? "";
  const srcName = parts[2] ?? "";
  try {
    await templates.createFromJob(form.value.name.trim() || t("tpl.autoName", { name: srcName }), {
      cluster,
      namespace: ns,
      name: srcName,
    });
    notify(t("tpl.generated"), "ok", 6000);
    genOpen.value = false;
    await tpl.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function remove(item: TemplateItem) {
  if (
    !(await confirmDialog({
      message: t("tpl.deleteConfirmName", { name: item.name }),
      danger: true,
    }))
  )
    return;
  try {
    await templates.remove(item.id);
    await tpl.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

function openEdit(t: TemplateItem) {
  editForm.value = { id: t.id, name: t.name, payload: JSON.stringify(t.payload, null, 2) };
  editOpen.value = true;
}

async function saveEdit() {
  if (!editForm.value.name.trim()) {
    notify(t("tpl.nameRequired"), "warn");
    return;
  }
  let payload: Record<string, unknown>;
  try {
    payload = JSON.parse(editForm.value.payload);
  } catch (e) {
    notify(t("tpl.payloadInvalid", { err: (e as Error).message }), "danger");
    return;
  }
  try {
    await templates.update(editForm.value.id, { name: editForm.value.name.trim(), payload });
    notify(t("tpl.updated"), "ok");
    editOpen.value = false;
    await tpl.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

function copyPayload() {
  const text = JSON.stringify(picked.value?.payload ?? {}, null, 2);
  void navigator.clipboard?.writeText(text);
  notify(t("tpl.copied"), "ok");
}

const cols = [
  { key: "name", label: t("tpl.col.template") },
  { key: "sourceJob", label: t("tpl.col.source") },
  { key: "createdAt", label: t("tpl.col.createdAt") },
  { key: "op", label: "", align: "right" as const, sortable: false },
];
</script>

<template>
  <PageHead
    :title="t('page.templates')"
    :sub="t('tpl.sub', { templates: tplItems.length, images: images.length })"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'tpl', label: t('tpl.tab.templates'), badge: tplItems.length },
      { key: 'img', label: t('tpl.tab.images'), badge: images.length },
      { key: 'conn', label: t('tpl.tab.connectors') },
    ]"
  />

  <DataTable
    v-if="tab === 'tpl'"
    :columns="cols"
    :rows="tplItems"
    row-key="id"
    :empty="t('tpl.empty')"
  >
    <template #cell-name="{ row }"
      ><span class="font-medium">{{ row.name }}</span></template
    >
    <template #cell-sourceJob="{ row }"
      ><span class="font-mono text-2xs text-fg-2">{{
        row.sourceJob ?? t("tpl.handwritten")
      }}</span></template
    >
    <template #cell-createdAt="{ row }"
      ><span class="font-mono text-3xs text-fg-2">{{ fmtLocal(row.createdAt) }}</span></template
    >
    <template #cell-op="{ row }">
      <div class="flex justify-end gap-2">
        <button
          class="text-2xs text-primary hover:underline"
          @click="
            picked = row as unknown as TemplateItem;
            viewOpen = true;
          "
        >
          {{ t("tpl.view") }}
        </button>
        <button
          class="text-2xs text-primary hover:underline"
          @click="openEdit(row as unknown as TemplateItem)"
        >
          {{ t("tpl.edit") }}
        </button>
        <button
          class="text-2xs text-danger hover:underline"
          @click="remove(row as unknown as TemplateItem)"
        >
          {{ t("tpl.delete") }}
        </button>
      </div>
    </template>
  </DataTable>

  <Card
    v-else-if="tab === 'img'"
    :title="t('tpl.imagesCard')"
    :hint="t('tpl.imagesHint', { n: images.length, bad: mutableCount })"
    :pad="false"
  >
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("tpl.col.image") }}</th>
            <th class="px-3 py-2">tag</th>
            <th class="px-3 py-2">{{ t("tpl.col.flink") }}</th>
            <th class="px-3 py-2 text-right">{{ t("tpl.col.jobs") }}</th>
            <th class="px-3 py-2">{{ t("tpl.col.example") }}</th>
            <th class="px-3 py-2">{{ t("tpl.col.traceable") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="i in images" :key="i.img" class="border-b border-line-2/60 last:border-0">
            <td class="max-w-[300px] truncate px-3 py-1.5 font-mono text-2xs" :title="i.img">
              {{ i.img }}
            </td>
            <td class="px-3 py-1.5 font-mono">{{ i.tag || "—" }}</td>
            <td class="px-3 py-1.5 font-mono">{{ i.ver || "—" }}</td>
            <td class="px-3 py-1.5 text-right font-mono">{{ i.n }}</td>
            <td
              class="max-w-[220px] truncate px-3 py-1.5 font-mono text-3xs text-fg-2"
              :title="i.jobs.join(', ')"
            >
              {{ i.jobs.join(", ") }}
            </td>
            <td class="px-3 py-1.5">
              <span
                class="rounded-full px-1.5 py-0.5 text-3xs"
                :class="i.mutable ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'"
                >{{ i.mutable ? "mutable tag" : "digest" }}</span
              >
            </td>
          </tr>
          <tr v-if="!images.length">
            <td colspan="6" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("tpl.noJobs") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p class="border-t border-line-2 px-3 py-2 text-2xs text-fg-2">{{ t("tpl.tagNote") }}</p>
  </Card>

  <div v-else class="grid gap-3">
    <Card
      :title="t('tpl.connCard')"
      :hint="t('tpl.connHint', { n: connList.length, total: CONNECTORS.length })"
    >
      <template #actions>
        <input
          v-model="connKw"
          :placeholder="t('tpl.connFilter')"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1 text-2xs"
        />
      </template>
      <div class="max-h-[420px] overflow-auto">
        <table class="w-full text-2xs">
          <thead class="sticky top-0 bg-surface text-left text-3xs text-fg-2">
            <tr>
              <th class="px-3 py-1.5 font-normal">{{ t("tpl.col.connector") }}</th>
              <th class="px-3 py-1.5 font-normal">artifactId</th>
              <th class="px-3 py-1.5 font-normal">{{ t("tpl.col.license") }}</th>
              <th class="px-3 py-1.5 font-normal">{{ t("tpl.col.bundled") }}</th>
              <th class="px-3 py-1.5 font-normal"></th>
            </tr>
          </thead>
          <tbody class="divide-y divide-line-2/60">
            <tr v-for="c in connList" :key="c.id">
              <td class="px-3 py-1.5">
                <div class="font-medium">{{ c.name }}</div>
                <div class="text-3xs text-fg-2">{{ c.note }}</div>
              </td>
              <td class="px-3 py-1.5 font-mono text-3xs text-fg-2">{{ c.dir }}</td>
              <td class="px-3 py-1.5">
                <span
                  class="rounded-full px-1.5 py-0.5 text-3xs"
                  :class="c.license.startsWith('GPL') ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'"
                  >{{ c.license }}</span
                >
              </td>
              <td class="px-3 py-1.5 text-fg-2">
                {{ c.bundled ? t("tpl.bundledYes") : t("tpl.bundledNo") }}
              </td>
              <td class="px-3 py-1.5 text-right">
                <button class="text-primary hover:underline" @click="copyCoord(c)">
                  {{ t("tpl.copyCoord") }}
                </button>
              </td>
            </tr>
            <tr v-if="!connList.length">
              <td colspan="5" class="px-3 py-6 text-center text-fg-2">{{ t("tpl.noConn") }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="border-t border-line-2 px-3 py-2 text-3xs text-fg-2">{{ t("tpl.licenseNote") }}</p>
    </Card>
  </div>

  <Modal
    v-model="importOpen"
    :title="t('tpl.import.title')"
    :sub="t('tpl.import.sub')"
    width="620px"
  >
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.name") }}</span>
        <input
          v-model="importName"
          placeholder="demo-job / kafka-etl"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.import.yaml") }}</span>
        <textarea
          v-model="importYaml"
          rows="14"
          spellcheck="false"
          placeholder="apiVersion: flink.apache.org/v1beta1&#10;kind: FlinkDeployment&#10;metadata: { name: demo-job }&#10;spec:&#10;  image: registry.example.com/flink/flink-base:1.17&#10;  ..."
          class="w-full rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-3xs leading-relaxed"
        />
      </label>
      <p v-if="importMsg" class="rounded-lg bg-danger/10 px-2.5 py-2 text-2xs text-danger">
        {{ importMsg }}
      </p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="importOpen = false"
      >
        {{ t("tpl.cancel") }}
      </button>
      <button
        :disabled="importBusy"
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
        @click="doImport"
      >
        {{ importBusy ? t("tpl.importing") : t("tpl.importButton") }}
      </button>
    </template>
  </Modal>

  <Modal v-model="genOpen" :title="t('tpl.gen.title')" :sub="t('tpl.gen.sub')" width="600px">
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.gen.source") }}</span>
        <select
          v-model="form.job"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        >
          <option value="">{{ t("tpl.gen.pick") }}</option>
          <option
            v-for="j in jobItems"
            :key="`${j.cluster}/${j.namespace}/${j.name}`"
            :value="`${j.cluster}/${j.namespace}/${j.name}`"
          >
            {{ j.namespace }} / {{ j.name }}
          </option>
        </select></label
      >
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.name") }}</span>
        <input
          v-model="form.name"
          :placeholder="t('tpl.gen.namePlaceholder')"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
      /></label>
      <p class="text-2xs text-fg-2">{{ t("tpl.gen.what") }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="genOpen = false"
      >
        {{ t("tpl.cancel") }}
      </button>
      <button
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
        @click="generate"
      >
        {{ t("tpl.gen.button") }}
      </button>
    </template>
  </Modal>

  <Modal v-model="editOpen" :title="t('tpl.edit.title')" :sub="editForm.id" width="760px">
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.name") }}</span>
        <input
          v-model="editForm.name"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("tpl.edit.payload") }}</span>
        <textarea
          v-model="editForm.payload"
          rows="16"
          spellcheck="false"
          class="rounded-lg border border-line bg-[#f8fafc] px-3 py-2 font-mono text-2xs leading-relaxed"
        />
      </label>
      <p class="text-2xs text-fg-2">{{ t("tpl.edit.note") }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="editOpen = false"
      >
        {{ t("tpl.cancel") }}
      </button>
      <button
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
        @click="saveEdit"
      >
        {{ t("tpl.save") }}
      </button>
    </template>
  </Modal>

  <Modal v-model="viewOpen" :title="t('tpl.content.title')" :sub="picked?.name ?? ''" width="720px">
    <pre
      class="max-h-[440px] overflow-auto rounded-lg bg-[#f8fafc] p-3 font-mono text-2xs leading-relaxed"
      >{{ JSON.stringify(picked?.payload ?? {}, null, 2) }}</pre>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="copyPayload"
      >
        {{ t("tpl.copyJson") }}
      </button>
      <button
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
        @click="viewOpen = false"
      >
        {{ t("tpl.close") }}
      </button>
    </template>
  </Modal>
</template>
