<script setup lang="ts">
import { confirmDialog, promptDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useQuery } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import Modal from "../components/Modal.vue";
import PageActions from "../components/PageActions.vue";
import Tabs from "../components/Tabs.vue";
import Card from "../components/Card.vue";
import EmptyState from "../components/EmptyState.vue";
import Badge from "../components/Badge.vue";
import { sessions } from "../api/client";
import { download, notify } from "../lib/notify";
import type { SqlJob } from "../api/types";
import type { SessionCluster } from "../api/client";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

const tab = ref("editor");
const picked = ref("");
const executing = ref(false);
const execErr = ref("");
const sqlJobs = ref<any[]>([]);
const editingJob = ref<SqlJob | null>(null); // 正在编辑的 SQL 作业（null = 新建）

type ExecStatement = { index: number; kind: string; rows: number; jobId: string | null };
type ExecResult = {
  columns: string[];
  rows: unknown[][];
  rowCount: number;
  tookMs: number;
  truncated: boolean;
  gateway: string;
  statements?: ExecStatement[];
  jobId?: string | null;
};
const result = ref<ExecResult | null>(null);

// ---------------------------------------------------------------- 会话集群
const q = useQuery({ queryKey: ["sessions"], queryFn: () => sessions.list() });
const items = computed<SessionCluster[]>(() => q.data?.value?.items ?? []);
const cur = computed(() => items.value.find((s) => s.name === picked.value));

const STORE_KEY = "flinkops.sql.draft";
const sql = ref(localStorage.getItem(STORE_KEY) ?? t("sql.exampleSql"));
const GATEWAY_MIN_VERSION = 1.16;

// ---------------------------------------------------------------- SQL 作业
async function loadSqlJobs() {
  try {
    const r = await fetch("/api/sql/jobs");
    const j = await r.json();
    sqlJobs.value = j.items ?? [];
  } catch {
    /* 忽略：作业列表拉不到不影响编辑器 */
  }
}

async function saveAsJob() {
  const text = (sql.value || "").trim();
  if (!text) {
    notify(t("sql.empty"), "warn");
    return;
  }
  if (!picked.value) {
    notify(t("sql.pickSession"), "warn");
    return;
  }
  try {
    if (editingJob.value) {
      // 编辑已有作业 → 追加新版本
      const note = await promptDialog({ message: t("sql.notePrompt"), input: { value: "" } });
      if (note === null) return;
      const r = await fetch(`/api/sql/jobs/${editingJob.value.id}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ sqlText: text, note: note || undefined }),
      });
      const j = await r.json();
      if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
      notify(t("sql.savedVersion", { v: j.job.version }), "ok");
      editingJob.value = j.job;
    } else {
      const name = await promptDialog({
        message: t("sql.jobNamePrompt"),
        input: { value: t("sql.unnamedJob") },
      });
      if (!name) return;
      const r = await fetch("/api/sql/jobs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          name,
          cluster: cur.value?.cluster ?? "local",
          namespace: cur.value?.namespace ?? "flink",
          session: picked.value,
          sqlText: text,
        }),
      });
      const j = await r.json();
      if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
      notify(t("sql.created", { name }), "ok");
      editingJob.value = j.job;
    }
    await loadSqlJobs();
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
}

function loadIntoEditor(j: SqlJob) {
  sql.value = j.sqlText ?? "";
  picked.value = j.session ?? "";
  editingJob.value = j;
  tab.value = "editor";
  notify(t("sql.loaded", { name: j.name, v: String(j.version) }), "ok");
}

async function deployJob(j: SqlJob, opts: { version?: number; savepoint?: boolean } = {}) {
  const ver = opts.version ?? j.version;
  const label = opts.savepoint ? t("sql.upgradeWithSavepoint") : t("sql.deploy");
  if (
    !(await confirmDialog({
      message: t("sql.deployConfirm", {
        label,
        name: j.name,
        v: String(ver ?? ""),
        session: String(j.session ?? ""),
      }),
    }))
  )
    return;
  notify(t("sql.deploying", { label }), "info", 3000);
  try {
    const r = await fetch(`/api/sql/jobs/${j.id}/deploy`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ version: opts.version, savepoint: !!opts.savepoint }),
    });
    const out = await r.json();
    if (out.ok) {
      const sp = out.savepoint ? `，savepoint=${out.savepoint}` : "";
      notify(t("sql.deployed", { v: out.version, state: out.job.state, sp: sp ?? "" }), "ok", 8000);
    } else {
      notify(t("sql.deployFailed", { err: out.error ?? "" }), "danger", 9000);
    }
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
  await loadSqlJobs();
}

async function upgradeJob(j: SqlJob) {
  const v = await promptDialog({
    message: t("sql.upgradePrompt", { cur: String(j.version ?? "") }),
    input: { value: "" },
  });
  if (v === null) return;
  await deployJob(j, { version: v.trim() ? Number(v.trim()) : undefined, savepoint: true });
}

async function suspendJob(j: SqlJob) {
  if (!(await confirmDialog({ message: t("sql.pauseConfirm", { name: j.name }) }))) return;
  try {
    const r = await fetch(`/api/sql/jobs/${j.id}/suspend`, { method: "POST" });
    const out = await r.json();
    notify(
      out.ok
        ? t("sql.paused", { state: out.job.state })
        : t("sql.pauseFailed", { err: out.error ?? "" }),
      out.ok ? "ok" : "danger",
    );
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
  await loadSqlJobs();
}

async function removeJob(j: SqlJob) {
  if (
    !(await confirmDialog({
      message: t("sql.deleteConfirm", { name: j.name }),
      danger: true,
    }))
  )
    return;
  try {
    await fetch(`/api/sql/jobs/${j.id}`, { method: "DELETE" });
    notify(t("sql.deleted"), "ok");
    if (editingJob.value?.id === j.id) editingJob.value = null;
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
  await loadSqlJobs();
}

// ---------------------------------------------------------------- 执行
async function runSql() {
  if (!picked.value) {
    notify(t("sql.pickSession"), "warn");
    return;
  }
  const text = (sql.value || "").trim();
  if (!text) {
    notify(t("sql.empty"), "warn");
    return;
  }
  executing.value = true;
  execErr.value = "";
  result.value = null;
  try {
    const resp = await fetch("/api/sql/execute", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        cluster: cur.value?.cluster ?? undefined,
        namespace: cur.value?.namespace ?? undefined,
        session: picked.value,
        sql: text,
      }),
    });
    const r = await resp.json();
    if (!resp.ok) throw new Error(r?.error?.message ?? `HTTP ${resp.status}`);
    result.value = r as ExecResult;
    const n = r.statements?.length ?? 1;
    const job = r.jobId ? `, jobId=${r.jobId}` : "";
    notify(t("sql.execResult", { n, rows: r.rowCount ?? 0, ms: r.tookMs ?? 0, job }), "ok", 6000);
    tab.value = "result";
  } catch (e) {
    execErr.value = (e as Error).message;
    tab.value = "result";
    notify((e as Error).message, "danger", 9000);
  } finally {
    executing.value = false;
  }
}

function explainExecution() {
  // SQL Gateway 是 Flink 1.16（FLIP-163）才引入的独立进程；session cluster（JM+TM）不自带。
  const verRaw = String(cur.value?.flinkVersion ?? "").replace(/[^\d.]/g, "");
  const ver = Number(verRaw) || 0;
  if (!picked.value) {
    notify(t("sql.gatewayNeeds16"), "warn", 8000);
    return;
  }
  if (ver && ver < GATEWAY_MIN_VERSION) {
    notify(
      t("sql.versionTooOld", { cluster: picked.value, version: cur.value?.flinkVersion ?? "1.15" }),
      "warn",
      11000,
    );
    return;
  }
  notify(t("sql.gatewayOk", { cluster: picked.value }), "info", 9000);
}

// ---------------------------------------------------------------- Catalog 浏览
const catData = ref<any>(null);
const catLoading = ref(false);
const catPath = ref<{ catalog?: string; database?: string; table?: string }>({});

async function loadCatalog(path: { catalog?: string; database?: string; table?: string } = {}) {
  if (!picked.value) {
    notify(t("sql.pickSession"), "warn");
    return;
  }
  catLoading.value = true;
  catPath.value = path;
  try {
    const qs = new URLSearchParams();
    if (path.catalog) qs.set("catalog", path.catalog);
    if (path.database) qs.set("database", path.database);
    if (path.table) qs.set("table", path.table);
    const cluster = cur.value?.cluster ?? "local";
    const ns = cur.value?.namespace ?? "flink";
    const r = await fetch(`/api/sql/catalog/${cluster}/${ns}/${picked.value}?${qs}`);
    const j = await r.json();
    if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
    catData.value = j;
  } catch (e) {
    catData.value = { level: "error", error: (e as Error).message };
  } finally {
    catLoading.value = false;
  }
}

function fillSelect(cat: string, db: string, tbl: string) {
  sql.value = `SELECT * FROM \`${cat}\`.\`${db}\`.\`${tbl}\` LIMIT 100`;
  tab.value = "editor";
  notify(t("sql.filledSelect", { table: tbl }), "ok", 3000);
}

// ---------------------------------------------------------------- 外部数据源
const dsrcs = ref<any[]>([]);
const dsrcId = ref("");
const dsrcTables = ref<any[]>([]);
const dsrcDiff = ref<any>(null);
const dsrcBusy = ref(false);
const dsrcOpen = ref(false);
const dsrcForm = ref({ name: "", kind: "starrocks", dsn: "" });

async function loadDatasources() {
  try {
    const r = await fetch("/api/catalogs");
    const j = await r.json();
    dsrcs.value = j.items ?? [];
    if (!dsrcId.value && dsrcs.value.length) dsrcId.value = dsrcs.value[0].id;
    if (dsrcId.value) await loadDsrcTables();
  } catch {
    /* 忽略：没有数据源也能用 SQL 工作台 */
  }
}

async function loadDsrcTables() {
  if (!dsrcId.value) return;
  try {
    const r = await fetch(`/api/catalogs/${dsrcId.value}/tables`);
    const j = await r.json();
    dsrcTables.value = j.items ?? [];
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
}

function openDsrcForm() {
  console.log("[Sql] openDsrcForm called", { dsrcOpen: dsrcOpen.value, dsrcs: dsrcs.value.length });
  dsrcForm.value = { name: "", kind: "starrocks", dsn: "mysql://user:pass@host:9030/db" };
  dsrcOpen.value = true;
  console.log("[Sql] dsrcOpen =", dsrcOpen.value);
}

async function saveDatasource() {
  const f = dsrcForm.value;
  console.log("[Sql] saveDatasource", f);
  if (!f.name.trim() || !f.dsn.trim()) {
    notify(t("sql.needNameAndDsn"), "warn");
    return;
  }
  try {
    const r = await fetch("/api/catalogs", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: f.name.trim(), kind: f.kind, dsn: f.dsn.trim() }),
    });
    const j = await r.json();
    if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
    notify(t("sql.registered", { name: f.name }), "ok", 6000);
    dsrcOpen.value = false;
    dsrcId.value = j.id;
    await loadDatasources();
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  }
}

async function syncDatasource() {
  if (!dsrcId.value) return;
  dsrcBusy.value = true;
  try {
    const r = await fetch(`/api/catalogs/${dsrcId.value}/sync`, { method: "POST" });
    const j = await r.json();
    if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
    notify(t("sql.synced", { n: j.tables ?? 0 }), "ok", 5000);
    await loadDsrcTables();
    dsrcDiff.value = null;
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  } finally {
    dsrcBusy.value = false;
  }
}

async function checkDiff() {
  if (!dsrcId.value) return;
  dsrcBusy.value = true;
  try {
    const r = await fetch(`/api/catalogs/${dsrcId.value}/diff`);
    const j = await r.json();
    if (!r.ok) throw new Error(j?.error?.message ?? `HTTP ${r.status}`);
    dsrcDiff.value = j;
    const total = (j.added?.length ?? 0) + (j.removed?.length ?? 0) + (j.changed?.length ?? 0);
    notify(
      total ? t("sql.changes", { n: total }) : t("sql.noChanges"),
      total ? "warn" : "ok",
      5000,
    );
  } catch (e) {
    notify((e as Error).message, "danger", 8000);
  } finally {
    dsrcBusy.value = false;
  }
}

async function pickDatasource(id: string) {
  dsrcId.value = id;
  dsrcDiff.value = null;
  await loadDsrcTables();
}

// ---------------------------------------------------------------- 导出
async function downloadExcel() {
  const r = result.value;
  if (!r) return;
  try {
    const resp = await fetch("/api/export/xlsx", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ columns: r.columns, rows: r.rows, sheet: "result" }),
    });
    if (!resp.ok) {
      const j = await resp.json().catch(() => null);
      throw new Error(j?.error?.message ?? `HTTP ${resp.status}`);
    }
    const blob = await resp.blob();
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `sql-result-${Date.now()}.xlsx`;
    a.click();
    URL.revokeObjectURL(url);
    notify(t("sql.exportedExcel", { n: r.rowCount }), "ok");
  } catch (e) {
    notify(t("sql.exportExcelFailed", { err: (e as Error).message }), "danger", 8000);
  }
}

function downloadResult() {
  const r = result.value;
  if (!r) return;
  const head = r.columns.join(",");
  const body = r.rows.map((row) =>
    row.map((c) => (c === null ? "" : String(c).replace(/,/g, ";"))).join(","),
  );
  download(`sql-result-${Date.now()}.csv`, [head, ...body].join("\n"), "text/csv;charset=utf-8");
  notify(t("sql.exported", { n: r.rowCount }), "ok");
}

// ---------------------------------------------------------------- 页头动作
const pageActions = computed(() => [
  {
    label: t("sql.save"),
    run: () => {
      localStorage.setItem(STORE_KEY, sql.value);
      notify(t("sql.draftSaved"), "ok");
    },
  },
  {
    label: t("sql.format"),
    run: () => {
      const text = sql.value
        .replace(/\s+/g, " ")
        .replace(
          /\b(select|from|where|group by|order by|left join|inner join|join|on|having|limit|insert into|values|with)\b/gi,
          (m) => `\n${m.toUpperCase()}`,
        )
        .replace(/\n\s*/g, "\n")
        .trim();
      if (text) {
        sql.value = text;
        notify(t("sql.formatted"), "ok");
      }
    },
  },
  { label: t("sql.saveAsJob"), run: saveAsJob },
  { label: t("sql.explain"), run: explainExecution },
  { label: t("sql.downloadCsv"), run: downloadResult },
]);

void loadSqlJobs();
void loadDatasources();
</script>

<template>
  <Modal v-model="dsrcOpen" :title="t('sql.source.title')" :sub="t('sql.source.sub')" width="620px">
    <div class="flex flex-col gap-2.5 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sql.source.name") }}</span>
        <input
          v-model="dsrcForm.name"
          :placeholder="t('sql.source.namePlaceholder')"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5"
      /></label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("sql.source.type") }}</span>
        <select
          v-model="dsrcForm.kind"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        >
          <option value="starrocks">{{ t("sql.src.starrocks") }}</option>
          <option value="doris">{{ t("sql.src.doris") }}</option>
          <option value="mysql">MySQL</option>
          <option value="postgres">{{ t("sql.src.postgres") }}</option>
          <option value="mongodb">{{ t("sql.src.mongo") }}</option>
        </select></label
      >
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">DSN</span>
        <input
          v-model="dsrcForm.dsn"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
      /></label>
      <p class="text-2xs text-fg-2">{{ t("sql.source.note") }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="dsrcOpen = false"
      >
        {{ t("sql.cancel") }}
      </button>
      <button
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white"
        @click="saveDatasource"
      >
        {{ t("sql.register") }}
      </button>
    </template>
  </Modal>
  <PageHead :title="t('page.sql')" :sub="t('sql.sub')" :badge="t('sql.badge')" tone="info">
    <template #actions
      ><PageActions :items="pageActions" />
      <select v-model="picked" class="rounded-lg border border-line bg-surface-2 px-2 py-1 text-xs">
        <option value="">{{ t("sql.pickSession") }}</option>
        <option v-for="s in items" :key="s.name" :value="s.name">
          {{ s.namespace }} / {{ s.name }}
        </option>
      </select>
      <RouterLink to="/sessions" class="rounded-lg bg-surface-3 px-3 py-1 text-xs hover:bg-hover">{{
        t("sql.manageSessions")
      }}</RouterLink>
    </template>
  </PageHead>

  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'editor', label: t('sql.tab.editor') },
      { key: 'catalog', label: 'Catalog' },
      { key: 'history', label: t('sql.tab.jobs'), badge: sqlJobs.length },
      { key: 'result', label: t('sql.tab.result') },
    ]"
  />

  <template v-if="tab === 'editor'">
    <div class="grid gap-3" style="grid-template-columns: minmax(0, 1fr) 300px">
      <Card
        title="SQL"
        :hint="
          cur
            ? t('sql.currentSession', { ns: cur.namespace, name: cur.name })
            : t('sql.noSessionSelected')
        "
      >
        <textarea
          v-model="sql"
          class="h-[320px] w-full resize-none rounded-lg border border-line bg-[#f8fafc] p-3 font-mono text-xs text-fg-1 outline-none"
          spellcheck="false"
        ></textarea>
        <div class="mt-2 flex items-center gap-2">
          <button
            :disabled="executing"
            class="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50"
            @click="runSql"
          >
            {{ executing ? t("sql.running") : t("sql.run") }}
          </button>
          <span class="text-2xs text-fg-2">{{ t("sql.gatewayNote") }}</span>
        </div>
      </Card>

      <Card :title="t('sql.sessionState')">
        <div v-if="cur" class="flex flex-col gap-1.5 text-xs">
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("sql.state") }}</span><Badge
              :tone="cur.state === 'RUNNING' ? 'ok' : 'warn'"
              dot
              >{{ cur.state }}</Badge
            >
          </div>
          <div class="flex justify-between">
            <span class="text-fg-2">{{ t("sql.tmPool") }}</span><span class="font-mono"
              >{{ cur.taskManagerReplicas }} × {{ cur.slotsPerTm }}</span
            >
          </div>
          <div class="flex justify-between">
            <span class="text-fg-2">JM</span
            ><span class="font-mono">{{ cur.jobManagerCpu }}C / {{ cur.jobManagerMemory }}</span>
          </div>
          <div class="flex justify-between">
            <span class="text-fg-2">SQL Gateway</span
            ><span class="font-mono">{{ cur.sqlGateway ?? t("sql.notDeployed") }}</span>
          </div>
          <a
            :href="`/ui/${cur.cluster}/${cur.namespace}/${cur.name}`"
            target="_blank"
            class="mt-1 text-primary hover:underline"
            >{{ t("sql.openUi") }}</a
          >
        </div>
        <p v-else class="text-xs text-fg-2">{{ t("sql.pickSessionHint") }}</p>
      </Card>
    </div>
  </template>

  <template v-else-if="tab === 'catalog'">
    <Card :title="t('sql.catalogCard')" :hint="picked ? `会话集群 ${picked}` : '先选会话集群'">
      <template #actions>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          :disabled="catLoading || !picked"
          @click="loadCatalog({})"
        >
          {{ catLoading ? t("sql.loading") : t("sql.loadCatalogs") }}
        </button>
      </template>

      <!-- 面包屑 -->
      <div class="mb-3 flex flex-wrap items-center gap-1 text-2xs">
        <button class="text-primary hover:underline" @click="loadCatalog({})">catalogs</button>
        <span v-if="catPath.catalog" class="text-fg-2">/</span>
        <button
          v-if="catPath.catalog"
          class="text-primary hover:underline"
          @click="loadCatalog({ catalog: catPath.catalog })"
        >
          {{ catPath.catalog }}
        </button>
        <span v-if="catPath.database" class="text-fg-2">/</span>
        <button
          v-if="catPath.database"
          class="text-primary hover:underline"
          @click="loadCatalog({ catalog: catPath.catalog, database: catPath.database })"
        >
          {{ catPath.database }}
        </button>
        <span v-if="catPath.table" class="text-fg-2">/</span>
        <span v-if="catPath.table" class="font-mono text-fg-1">{{ catPath.table }}</span>
      </div>

      <div
        v-if="catData?.level === 'error'"
        class="rounded-lg bg-danger/8 p-3 text-2xs text-danger"
      >
        {{ catData.error }}
      </div>

      <!-- 列表层 -->
      <div
        v-else-if="catData && ['catalogs', 'databases', 'tables'].includes(catData.level)"
        class="flex flex-col"
      >
        <button
          v-for="name in catData.items"
          :key="name"
          class="flex items-center gap-2 border-b border-line-2/50 py-1.5 text-left text-xs last:border-0 hover:bg-hover/40"
          @click="
            catData.level === 'catalogs'
              ? loadCatalog({ catalog: name })
              : catData.level === 'databases'
                ? loadCatalog({ catalog: catPath.catalog, database: name })
                : loadCatalog({ catalog: catPath.catalog, database: catPath.database, table: name })
          "
        >
          <span class="font-mono text-fg-1">{{ name }}</span>
          <span class="ml-auto flex items-center gap-2 text-3xs text-fg-2">
            <button
              v-if="catData.level === 'tables'"
              class="rounded border border-line px-1.5 py-0.5 text-3xs text-primary hover:bg-hover"
              @click.stop="fillSelect(catPath.catalog!, catPath.database!, name)"
            >
              {{ t("sql.queryData") }}
            </button>
            <span>{{
              catData.level === "catalogs"
                ? "catalog →"
                : catData.level === "databases"
                  ? "database →"
                  : t("sql.viewColumns")
            }}</span>
          </span>
        </button>
        <div v-if="!catData.items?.length" class="py-6 text-center text-xs text-fg-2">
          {{ catData.level === "tables" ? t("sql.noTables") : t("sql.noContent") }}
        </div>
      </div>

      <!-- 表结构 -->
      <div v-else-if="catData?.level === 'table'">
        <div class="mb-2 text-2xs text-fg-2">
          {{ t("sql.colCount", { n: catData.rows?.length ?? 0 }) }}
        </div>
        <table class="w-full text-xs">
          <thead class="bg-surface-2 text-3xs uppercase tracking-wider text-fg-2">
            <tr>
              <th v-for="c in catData.columns" :key="c" class="px-2 py-1.5 text-left font-medium">
                {{ c }}
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(row, i) in catData.rows" :key="i" class="border-t border-line-2/60">
              <td v-for="(cell, j) in row" :key="j" class="px-2 py-1 font-mono text-2xs">
                {{ cell }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <EmptyState v-else :title="t('sql.loadCatalogs')" :desc="t('sql.catalogDesc')" />
    </Card>

    <!-- 外部数据源元数据（StarRocks / MySQL / Doris） -->
    <Card
      :title="t('sql.sourcesCard')"
      :hint="t('sql.sourcesHint', { n: dsrcs.length })"
      class="mt-3"
    >
      <template #actions>
        <select
          :value="dsrcId"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1 text-2xs"
          @change="pickDatasource(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="d in dsrcs" :key="d.id" :value="d.id">
            {{ d.name }}（{{ d.dsnMasked }}）
          </option>
        </select>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover disabled:opacity-50"
          :disabled="dsrcBusy || !dsrcId"
          @click="syncDatasource"
        >
          {{ t("sql.syncMeta") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover disabled:opacity-50"
          :disabled="dsrcBusy || !dsrcId"
          @click="checkDiff"
        >
          {{ t("sql.detectChanges") }}
        </button>
        <button
          class="rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white"
          @click="openDsrcForm"
        >
          {{ t("sql.addNew") }}
        </button>
      </template>

      <div v-if="dsrcDiff" class="mb-3 rounded-lg bg-surface-2 p-2.5 text-2xs">
        <div class="mb-1 text-fg-2">
          {{ t("sql.live") }} {{ dsrcDiff.liveTables }} {{ t("sql.tablesCache") }}
          {{ dsrcDiff.cachedTables }}
          {{ t("sql.tablesAdded") }}
          {{ dsrcDiff.added?.length ?? 0 }} {{ t("sql.removedShort") }}
          {{ dsrcDiff.removed?.length ?? 0 }} {{ t("sql.changedShort") }}
          {{ dsrcDiff.changed?.length ?? 0 }}
        </div>
        <div v-for="a in dsrcDiff.added" :key="`a-${a.db}.${a.table}`" class="text-ok">
          + {{ a.db }}.{{ a.table }}（{{ t("sql.colsTailShort", { n: a.columns }) }}）
        </div>
        <div v-for="r in dsrcDiff.removed" :key="`r-${r.db}.${r.table}`" class="text-danger">
          - {{ r.db }}.{{ r.table }}
        </div>
        <div v-for="ch in dsrcDiff.changed" :key="`c-${ch.db}.${ch.table}`" class="text-warn">
          ~ {{ ch.db }}.{{ ch.table }}
          <span v-for="x in ch.added" :key="`ca-${x.name}`"> +{{ x.name }}({{ x.type }})</span>
          <span v-for="x in ch.removed" :key="`cr-${x.name}`"> -{{ x.name }}</span>
          <span v-for="x in ch.retyped" :key="`ct-${x.name}`">
            {{ x.name }}:{{ x.from }}→{{ x.to }}</span
          >
        </div>
      </div>

      <div v-if="dsrcTables.length" class="max-h-[280px] overflow-auto">
        <div
          v-for="t in dsrcTables"
          :key="`${t.db}.${t.table}`"
          class="flex items-center gap-2 border-b border-line-2/50 py-1 text-2xs last:border-0"
        >
          <button
            class="font-mono text-fg-1 hover:text-primary"
            @click="fillSelect(dsrcId, t.db, t.table)"
          >
            {{ t.db }}.{{ t.table }}
          </button>
          <span class="text-3xs text-fg-2">{{
            t("sql.colCount", { n: t.columns?.length ?? 0 })
          }}</span>
        </div>
      </div>
      <EmptyState v-else :title="t('sql.noMeta')" :desc="t('sql.noMetaDesc')" />
    </Card>
  </template>

  <template v-else-if="tab === 'history'">
    <Card :title="t('sql.jobsCard')" :hint="t('sql.jobsHint', { n: sqlJobs.length })">
      <template #actions>
        <button
          class="rounded-lg bg-primary px-2.5 py-1 text-2xs font-medium text-white"
          @click="saveAsJob"
        >
          {{
            editingJob
              ? t("sql.saveAsVersion", { v: (editingJob.version ?? 0) + 1 })
              : t("sql.saveEditorAsJob")
          }}
        </button>
      </template>
      <div v-if="sqlJobs.length" class="overflow-x-auto">
        <table class="w-full text-xs">
          <thead class="bg-surface-2 text-3xs uppercase tracking-wider text-fg-2">
            <tr>
              <th class="px-2 py-1.5 text-left font-medium">{{ t("sql.col.job") }}</th>
              <th class="px-2 py-1.5 text-left font-medium">{{ t("sql.col.version") }}</th>
              <th class="px-2 py-1.5 text-left font-medium">{{ t("sql.state") }}</th>
              <th class="px-2 py-1.5 text-left font-medium">{{ t("sql.col.session") }}</th>
              <th class="px-2 py-1.5 text-left font-medium">{{ t("sql.col.updatedAt") }}</th>
              <th class="px-2 py-1.5 text-right font-medium">{{ t("sql.col.actions") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="j in sqlJobs" :key="j.id" class="border-t border-line-2/60">
              <td class="px-2 py-1.5">{{ j.name }}</td>
              <td class="px-2 py-1.5 font-mono">v{{ j.version }}</td>
              <td class="px-2 py-1.5">
                <span
                  :class="
                    j.state === 'RUNNING'
                      ? 'text-ok'
                      : j.state === 'FAILED'
                        ? 'text-danger'
                        : 'text-fg-2'
                  "
                  >{{ j.state }}</span
                >
              </td>
              <td class="px-2 py-1.5 font-mono text-2xs text-fg-2">{{ j.session }}</td>
              <td class="px-2 py-1.5 font-mono text-3xs text-fg-2">
                {{ fmtLocal(j.updatedAt) }}
              </td>
              <td class="px-2 py-1.5 text-right">
                <button
                  class="mr-2 text-2xs text-primary hover:underline"
                  @click="loadIntoEditor(j)"
                >
                  {{ t("sql.edit") }}
                </button>
                <button class="mr-2 text-2xs text-ok hover:underline" @click="deployJob(j)">
                  {{ t("sql.deploy") }}
                </button>
                <button
                  v-if="j.jobId"
                  class="mr-2 text-2xs text-warn hover:underline"
                  @click="suspendJob(j)"
                >
                  {{ t("sql.pause") }}
                </button>
                <button class="mr-2 text-2xs text-info hover:underline" @click="upgradeJob(j)">
                  {{ t("sql.upgrade") }}
                </button>
                <button class="text-2xs text-danger hover:underline" @click="removeJob(j)">
                  {{ t("sql.delete") }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState v-else :title="t('sql.noJobs')" :desc="t('sql.noJobsDesc')" />
    </Card>
  </template>

  <template v-else>
    <Card
      v-if="result"
      :title="t('sql.resultTitle', { n: result.rowCount })"
      :hint="`${result.tookMs}ms · ${result.gateway}`"
    >
      <template #actions>
        <span v-if="result.truncated" class="text-2xs text-warn">{{ t("sql.truncated") }}</span>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="downloadExcel"
        >
          {{ t("sql.downloadExcel") }}
        </button>
        <button
          class="rounded-lg bg-surface-3 px-2.5 py-1 text-2xs hover:bg-hover"
          @click="downloadResult"
        >
          {{ t("sql.downloadCsv") }}
        </button>
      </template>
      <div class="overflow-x-auto">
        <table class="w-full text-xs">
          <thead class="bg-surface-2 text-3xs uppercase tracking-wider text-fg-2">
            <tr>
              <th class="px-2 py-1.5 text-left font-medium">#</th>
              <th v-for="c in result.columns" :key="c" class="px-2 py-1.5 text-left font-medium">
                {{ c }}
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(row, i) in result.rows" :key="i" class="border-t border-line-2/60">
              <td class="px-2 py-1 text-fg-2">{{ i + 1 }}</td>
              <td v-for="(cell, j) in row" :key="j" class="px-2 py-1 font-mono text-2xs">
                {{ cell === null ? "NULL" : cell }}
              </td>
            </tr>
            <tr v-if="!result.rows.length">
              <td :colspan="result.columns.length + 1" class="px-2 py-6 text-center text-fg-2">
                {{ t("sql.noRows") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>
    <Card v-else-if="execErr" :title="t('sql.execFailed')">
      <pre
        class="whitespace-pre-wrap rounded-lg bg-danger/8 p-3 font-mono text-2xs leading-relaxed text-danger"
        >{{ execErr }}</pre>
    </Card>
    <EmptyState v-else :title="t('sql.noResult')" :desc="t('sql.noResultDesc')" />
  </template>
</template>
