<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref, watchEffect } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink } from "vue-router";
import { admin, audit, auth, jobs, snapshots, system } from "../api/client";
import { download, notify } from "../lib/notify";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Kpi from "../components/Kpi.vue";
import Modal from "../components/Modal.vue";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

const licOpen = ref(false);
const sec = ref("s-general");
const SECTIONS = computed(() => [
  { key: "s-general", label: t("set.card.general") },
  { key: "s-defaults", label: t("set.card.defaults") },
  { key: "s-operator", label: t("set.card.operator") },
  { key: "s-db", label: t("set.card.db") },
  { key: "s-storage", label: t("set.card.state") },
  { key: "s-notify", label: t("set.card.notify") },
  { key: "s-security", label: t("set.card.security") },
  { key: "s-about", label: t("set.card.about") },
]);

const me = useQuery({ queryKey: ["me"], queryFn: auth.me });

// ---------------------------------------------------------------- 账号安全
const pwdOld = ref("");
const pwdNew = ref("");
const pwdBusy = ref(false);
const totpCode = ref("");
const totpPending = ref<{ secret: string; otpauth: string } | null>(null);
const totpBusy = ref(false);

async function changePassword() {
  if (pwdBusy.value) return;
  if (pwdNew.value.length < 8) {
    notify(t("set.pwd.tooShort"), "warn");
    return;
  }
  pwdBusy.value = true;
  try {
    await auth.changePassword(pwdOld.value, pwdNew.value);
    pwdOld.value = "";
    pwdNew.value = "";
    notify(t("set.pwd.updated"), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    pwdBusy.value = false;
  }
}

async function startTotp() {
  totpBusy.value = true;
  try {
    const r = await auth.totpSetup();
    totpPending.value = { secret: r.secret, otpauth: r.otpauth };
    totpCode.value = "";
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    totpBusy.value = false;
  }
}

async function confirmTotp() {
  if (totpBusy.value) return;
  totpBusy.value = true;
  try {
    await auth.totpEnable(totpCode.value.trim());
    totpPending.value = null;
    totpCode.value = "";
    await me.refetch();
    notify(t("set.totp.enabled"), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    totpBusy.value = false;
  }
}

async function disableTotp() {
  if (totpBusy.value) return;
  totpBusy.value = true;
  try {
    await auth.totpDisable(totpCode.value.trim());
    totpCode.value = "";
    await me.refetch();
    notify(t("set.totp.disabled"), "ok");
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    totpBusy.value = false;
  }
}
const caps = useQuery({ queryKey: ["cluster-info"], queryFn: system.clusterInfo, retry: false });
const all = useQuery({ queryKey: ["jobs"], queryFn: () => jobs.list(), refetchInterval: 30_000 });
const snaps = useQuery({
  queryKey: ["snapshots", "all"],
  queryFn: () => snapshots.list(),
  retry: false,
});
const logs = useQuery({
  queryKey: ["audit", "recent"],
  queryFn: () => audit.list(),
  refetchInterval: 15_000,
});
const cfg = useQuery({ queryKey: ["settings"], queryFn: admin.getSettings });
const back = useQuery({ queryKey: ["admin", "backup"], queryFn: admin.backup, retry: false });

const items = computed(() => all.data?.value?.items ?? []);
const cfgForm = ref({
  retention: 90,
  dbMax: 512,
  promUrl: "",
  runnerJar: "",
  runnerStyle: "sql-file",
});
const cfgLoaded = ref(false);
watchEffect(() => {
  const e = cfg.data?.value?.effective;
  if (e && !cfgLoaded.value) {
    cfgForm.value = {
      retention: Number(e.audit_retention_days ?? 90),
      dbMax: Number(e.db_max_mb ?? 512),
      promUrl: e.prometheus_url ?? "",
      runnerJar: e.sql_runner_jar ?? "",
      runnerStyle: e.sql_runner_args_style === "inline" ? "inline" : "sql-file",
    };
    cfgLoaded.value = true;
  }
});

// 本机偏好（只影响这个浏览器，不入库）
const pref = ref({
  name: localStorage.getItem("fo.name") ?? "FlinkOps",
  tz: localStorage.getItem("fo.tz") ?? "Asia/Shanghai",
  lang: localStorage.getItem("fo.lang") ?? "zh",
  refresh: Number(localStorage.getItem("fo.refresh") ?? 30),
  pageSize: Number(localStorage.getItem("fo.pageSize") ?? 20),
});
watchEffect(() => {
  localStorage.setItem("fo.name", pref.value.name);
  localStorage.setItem("fo.tz", pref.value.tz);
  localStorage.setItem("fo.lang", pref.value.lang);
  localStorage.setItem("fo.refresh", String(pref.value.refresh));
  localStorage.setItem("fo.pageSize", String(pref.value.pageSize));
});

async function saveCfg() {
  try {
    await admin.saveSettings({
      auditRetentionDays: cfgForm.value.retention,
      dbMaxMb: cfgForm.value.dbMax,
      prometheusUrl: cfgForm.value.promUrl,
      sqlRunnerJar: cfgForm.value.runnerJar.trim(),
      sqlRunnerArgsStyle: cfgForm.value.runnerStyle,
    });
    notify(t("set.saved"), "ok", 6000);
    cfgLoaded.value = false;
    await cfg.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function resetCfg() {
  if (!(await confirmDialog({ message: t("set.resetConfirm"), danger: true }))) return;
  try {
    const r = await admin.resetSettings();
    notify(r.note, "ok");
    cfgLoaded.value = false;
    await cfg.refetch();
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

const pageActions = computed(() => [
  { label: t("set.resetDefaults"), run: resetCfg },
  { label: t("set.saveAll"), run: saveCfg },
  {
    label: t("set.vacuum"),
    run: async () => {
      try {
        const r = await admin.vacuum();
        const kb = (n?: number) => (n == null ? "—" : (n / 1024).toFixed(0) + " KB");
        notify(`${r.note}：${kb(r.beforeBytes)} → ${kb(r.afterBytes)}`, r.vacuumed ? "ok" : "warn");
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("set.exportBackup"),
    run: async () => {
      try {
        const r = await admin.backup();
        notify(
          r.kind === "sqlite"
            ? t("set.backupDone", {
                path: String(r.path ?? ""),
                kb: ((r.sizeBytes ?? 0) / 1024).toFixed(0),
              })
            : r.hint,
          "info",
          8000,
        );
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("set.diagnostics"),
    run: async () => {
      try {
        const d = await admin.diagnostic();
        download(
          `flinkops-diag-${Date.now()}.json`,
          JSON.stringify(d, null, 2),
          "application/json;charset=utf-8",
        );
        notify(t("set.diagnosticsDone"), "ok");
      } catch (e) {
        notify((e as Error).message, "danger");
      }
    },
  },
  {
    label: t("set.viewLicenses"),
    run: () => {
      licOpen.value = true;
    },
  },
]);

// 状态存储：从快照 path 里归纳 distinct 前缀
const storagePrefixes = computed(() => {
  const m = new Map<string, number>();
  for (const s of snaps.data?.value?.items ?? []) {
    const p = String(s.path ?? "");
    const prefix = p.includes("/savepoint/") ? p.split("/savepoint/")[0] + "/savepoint" : p || "—";
    m.set(prefix, (m.get(prefix) ?? 0) + 1);
  }
  return [...m.entries()].map(([path, n]) => ({ path, n }));
});

const capsData = computed(() => caps.data?.value);
const operatorLines = computed(() =>
  (capsData.value?.clusters ?? []).flatMap((c) =>
    c.operators.map((o) => ({ cluster: c.id, ...o })),
  ),
);
const jobStats = computed(() => capsData.value?.jobs);

const recentLogs = computed(() => (logs.data?.value?.items ?? []).slice(0, 8));

function go(key: string) {
  sec.value = key;
  document.getElementById(key)?.scrollIntoView({ behavior: "smooth", block: "start" });
}
</script>

<template>
  <h1 class="mb-1 text-[20px] font-semibold">{{ t("set.title") }}</h1>
  <p class="mb-3 text-xs text-fg-2">{{ t("set.sub") }}</p>
  <div class="mb-4"><PageActions :items="pageActions" /></div>

  <div class="grid gap-4 xl:grid-cols-[176px_1fr]">
    <!-- 左侧导航 -->
    <nav class="flex flex-wrap gap-1 xl:sticky xl:top-3 xl:h-fit xl:flex-col">
      <button
        v-for="s in SECTIONS"
        :key="s.key"
        class="rounded-lg px-2.5 py-1.5 text-left text-xs transition"
        :class="
          sec === s.key
            ? 'bg-primary/10 font-medium text-primary'
            : 'text-fg-2 hover:bg-hover hover:text-fg'
        "
        @click="go(s.key)"
      >
        {{ s.label }}
      </button>
    </nav>

    <div class="flex flex-col gap-3">
      <!-- 常规 -->
      <div id="s-general">
        <Card :title="t('set.general.title')" :hint="t('set.general.hint')">
          <div
            class="grid gap-3"
            style="grid-template-columns: repeat(auto-fit, minmax(200px, 1fr))"
          >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.name") }}</span>
              <input
                v-model="pref.name"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1"
            /></label>
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.defaultCluster") }}</span>
              <select class="rounded-lg border border-line bg-surface-2 px-2 py-1">
                <option v-for="c in me.data?.value?.clusters ?? []" :key="c" :value="c">
                  {{ c }}
                </option>
              </select></label
            >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.tz") }}</span>
              <select
                v-model="pref.tz"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              >
                <option value="Asia/Shanghai">Asia/Shanghai (UTC+8)</option>
                <option value="UTC">UTC</option>
              </select></label
            >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.lang") }}</span>
              <select
                v-model="pref.lang"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1"
              >
                <option value="zh">{{ t("set.general.langZh") }}</option>
                <option value="en">English</option>
              </select></label
            >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.refresh") }}</span>
              <select
                v-model.number="pref.refresh"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              >
                <option :value="10">{{ t("set.sec10") }}</option>
                <option :value="30">{{ t("set.sec30") }}</option>
                <option :value="60">{{ t("set.sec60") }}</option>
              </select></label
            >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.general.pageSize") }}</span>
              <select
                v-model.number="pref.pageSize"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              >
                <option :value="10">10</option>
                <option :value="20">20</option>
                <option :value="50">50</option>
              </select></label
            >
          </div>
          <p class="mt-2 text-2xs text-fg-2">
            {{ t("set.general.notePrefix") }}
            <span class="font-mono">FLINKOPS_*</span>{{ t("set.general.noteSuffix") }}
          </p>
        </Card>
      </div>

      <!-- 作业默认值 -->
      <div id="s-defaults">
        <Card :title="t('set.defaults.title')" :hint="t('set.defaults.hint')">
          <div
            class="grid gap-3"
            style="grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)) text-xs"
          >
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.version") }}</div>
              <div class="mt-0.5 font-mono">
                {{
                  [...new Set(items.map((j) => j.flinkVersion).filter(Boolean))].join(" / ") || "—"
                }}
              </div>
            </div>
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.image") }}</div>
              <div class="mt-0.5 truncate font-mono text-3xs" :title="items[0]?.image">
                {{ [...new Set(items.map((j) => j.image).filter(Boolean))].length }}
                {{ t("set.defaults.imageInUse") }}
              </div>
            </div>
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.parallelism") }}</div>
              <div class="mt-0.5 font-mono">{{ items[0]?.parallelism ?? "—" }}</div>
            </div>
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.backend") }}</div>
              <div class="mt-0.5 font-mono">rocksdb</div>
            </div>
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.upgrade") }}</div>
              <div class="mt-0.5 font-mono">
                {{
                  [...new Set(items.map((j) => j.upgradeMode).filter(Boolean))].join(" / ") || "—"
                }}
              </div>
            </div>
            <div class="rounded-lg border border-line-2 px-3 py-2">
              <div class="text-fg-2">{{ t("set.defaults.ckpt") }}</div>
              <div class="mt-0.5 font-mono">{{ t("set.defaults.ckptNote") }}</div>
            </div>
          </div>
          <p class="mt-2 text-2xs text-fg-2">
            {{ t("set.defaults.notePrefix") }}
            <RouterLink to="/presets" class="text-primary hover:underline">{{
              t("page.presets")
            }}</RouterLink>{{ t("set.defaults.noteSuffix") }}
          </p>
        </Card>
      </div>

      <!-- Operator 行为 -->
      <div id="s-operator">
        <Card
          :title="t('set.operator.title')"
          :hint="
            operatorLines.length
              ? t('set.operatorCount', { n: operatorLines.length })
              : t('set.operatorNone')
          "
        >
          <div
            v-for="op in operatorLines"
            :key="op.cluster + op.name"
            class="mb-2 rounded-lg border border-line-2 px-3 py-2 last:mb-0"
          >
            <div class="flex flex-wrap items-center gap-2 text-xs">
              <span
                class="h-1.5 w-1.5 rounded-full"
                :class="op.readyReplicas ? 'bg-ok' : 'bg-danger'"
              ></span>
              <span class="font-mono">{{ op.cluster }}/{{ op.namespace }}/{{ op.name }}</span>
              <span class="text-fg-2">ready {{ op.readyReplicas }}</span>
              <span class="ml-auto break-all font-mono text-3xs text-fg-2">{{
                op.images[0] ?? "—"
              }}</span>
            </div>
          </div>
          <div
            class="grid gap-2 text-xs"
            style="grid-template-columns: repeat(auto-fit, minmax(180px, 1fr))"
          >
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.operator.reconcile") }}</span><span
                class="font-mono"
                >{{ t("set.operator.reconcileNote") }}</span
              >
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.operator.autoscaler") }}</span><span
                class="font-mono"
                >{{ t("set.operator.autoscalerNote") }}</span
              >
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.operator.crds") }}</span><span class="font-mono"
                >{{ (capsData?.clusters?.[0]?.supportedKinds ?? []).length }}
                {{ t("set.operator.kinds") }}</span
              >
            </div>
          </div>
          <div class="mt-2 flex flex-wrap gap-1">
            <span
              v-for="k in capsData?.clusters?.[0]?.supportedKinds ?? []"
              :key="k"
              class="rounded-full border border-line bg-surface-3 px-1.5 py-0.5 text-3xs text-fg-2"
              >{{ k }}</span
            >
          </div>
          <p
            v-if="jobStats"
            class="mt-2 rounded-lg px-3 py-2 text-2xs"
            :class="
              jobStats.withoutObservedGeneration ? 'bg-warn/10 text-warn' : 'bg-ok/10 text-ok'
            "
          >
            {{ jobStats.note }}（{{ jobStats.withConditions }}/{{ jobStats.total }}
            {{ t("set.operator.withConditions") }}
          </p>
        </Card>
      </div>

      <!-- 数据库与保留期 -->
      <div id="s-db">
        <Card
          :title="t('set.db.title')"
          :hint="
            back.data?.value
              ? `${back.data.value.kind} · ${((back.data.value.sizeBytes ?? 0) / 1024).toFixed(0)} KB`
              : ''
          "
        >
          <div
            class="mb-3 grid gap-3"
            style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr))"
          >
            <Kpi
              :label="t('set.db.engine')"
              :value="back.data?.value?.kind ?? '—'"
              :foot="back.data?.value?.path ?? ''"
            />
            <Kpi
              :label="t('set.db.auditRetention')"
              :value="t('set.days', { n: cfgForm.retention })"
              :foot="t('set.hourly')"
            />
            <Kpi :label="t('set.db.dbWarn')" :value="`${cfgForm.dbMax} MB`" />
            <Kpi
              :label="t('set.db.auditRows')"
              :value="logs.data?.value?.items?.length ?? 0"
              :foot="t('set.rowsMax')"
            />
          </div>
          <div
            class="grid gap-3"
            style="grid-template-columns: repeat(auto-fit, minmax(200px, 1fr))"
          >
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.db.retentionDays") }}</span>
              <input
                v-model.number="cfgForm.retention"
                type="number"
                min="1"
                max="3650"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
            /></label>
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.db.warnMb") }}</span>
              <input
                v-model.number="cfgForm.dbMax"
                type="number"
                min="16"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
            /></label>
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.db.prometheus") }}</span>
              <input
                v-model="cfgForm.promUrl"
                placeholder="http://prometheus.monitoring.svc:9090"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
            /></label>
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">SQL runner jar</span>
              <input
                v-model="cfgForm.runnerJar"
                placeholder="local:///opt/flink/usrlib/sql-runner.jar"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
            /></label>
            <label class="flex flex-col gap-1 text-xs"
              ><span class="text-fg-2">{{ t("set.db.runner") }}</span>
              <select
                v-model="cfgForm.runnerStyle"
                class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              >
                <option value="sql-file">{{ t("set.db.runnerDefault") }}</option>
                <option value="inline">inline</option>
              </select></label
            >
          </div>
          <p class="mt-2 text-2xs text-fg-2">{{ t("set.db.note") }}</p>
        </Card>
      </div>

      <!-- 状态存储 -->
      <div id="s-storage">
        <Card :title="t('set.state.title')" :hint="t('set.state.hint')">
          <div class="flex flex-col gap-2 text-xs">
            <div
              v-for="p in storagePrefixes"
              :key="p.path"
              class="flex items-center gap-2 rounded-lg border border-line-2 px-3 py-1.5"
            >
              <span class="truncate font-mono text-2xs" :title="p.path">{{ p.path }}</span>
              <span class="ml-auto shrink-0 font-mono text-3xs text-fg-2">{{
                t("set.state.snapshots", { n: p.n })
              }}</span>
            </div>
            <p v-if="!storagePrefixes.length" class="text-fg-2">{{ t("set.state.empty") }}</p>
          </div>
          <p class="mt-2 text-2xs leading-relaxed text-fg-2">
            {{ t("set.state.currentDir")
            }}<span class="font-mono"
              >hdfs://encd/user/flink/{checkpoint,savepoint}/&lt;job&gt;</span
            >{{ t("set.state.noteTail") }} <span class="font-mono">state.checkpoints.dir</span> /
            <span class="font-mono">state.savepoints.dir</span>。
          </p>
        </Card>
      </div>

      <!-- 通知 -->
      <div id="s-notify">
        <Card :title="t('set.notify.title')" :hint="t('set.notify.hint')">
          <p class="text-xs leading-relaxed text-fg-2">
            {{ t("set.notify.current") }}<strong>{{ t("set.notify.dbOnly") }}</strong
            >{{ t("set.notify.dbOnlyTail")
            }}<RouterLink to="/alerts" class="text-primary hover:underline">{{
              t("set.notify.alertsLink")
            }}</RouterLink
            >{{ t("set.notify.channels") }}<span class="font-mono">FLINKOPS_NOTIFY_*</span
            >{{ t("set.notify.notImplemented") }}
          </p>
          <p class="mt-1 text-2xs text-fg-2">{{ t("set.notify.order") }}</p>
        </Card>
      </div>

      <!-- 安全 -->
      <div id="s-security">
        <Card :title="t('set.security.title')" :hint="t('set.security.hint')">
          <div
            class="grid gap-2 text-xs"
            style="grid-template-columns: repeat(auto-fit, minmax(230px, 1fr))"
          >
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.security.auth") }}</span><span class="font-mono">{{
                t("set.security.localAccount", { user: me.data?.value?.user ?? "—" })
              }}</span>
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.security.mode") }}</span><span
                class="font-mono"
                :class="me.data?.value?.readonly ? 'text-warn' : 'text-ok'"
                >{{ me.data?.value?.readonly ? t("set.ro") : t("set.rw") }}</span
              >
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.security.sessions") }}</span><span
                class="font-mono"
                >{{ t("set.security.sessionsDb") }}</span
              >
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.security.audit") }}</span><span class="font-mono">{{
                t("set.security.auditLine", { n: cfgForm.retention })
              }}</span>
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">OIDC / LDAP</span
              ><span class="font-mono text-fg-2">{{ t("set.security.notWired") }}</span>
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">MFA（TOTP）</span
              ><span
                class="font-mono"
                :class="me.data?.value?.totpEnabled ? 'text-ok' : 'text-fg-2'"
                >{{ me.data?.value?.totpEnabled ? t("set.enabled") : t("set.disabled") }}</span
              >
            </div>
            <div class="flex justify-between rounded-lg border border-line-2 px-3 py-1.5">
              <span class="text-fg-2">{{ t("set.security.worm") }}</span><span
                class="font-mono text-fg-2"
                >{{ t("set.security.notWired") }}</span
              >
            </div>
          </div>
          <p class="mt-2 text-2xs text-fg-2">
            {{ t("set.security.gate")
            }}<RouterLink to="/users" class="text-primary hover:underline">{{
              t("set.security.membersLink")
            }}</RouterLink
            >）。
          </p>

          <div class="mt-4 grid gap-4 border-t border-line-2 pt-3 md:grid-cols-2">
            <!-- 改密码 -->
            <div>
              <div class="mb-2 text-xs font-medium">{{ t("set.pwd.change") }}</div>
              <div class="flex flex-col gap-2">
                <input
                  v-model="pwdOld"
                  type="password"
                  autocomplete="current-password"
                  :placeholder="t('set.pwd.current')"
                  class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 text-xs"
                />
                <input
                  v-model="pwdNew"
                  type="password"
                  autocomplete="new-password"
                  :placeholder="t('set.pwd.new')"
                  class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 text-xs"
                />
                <button
                  class="self-start rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
                  :disabled="pwdBusy"
                  @click="changePassword"
                >
                  {{ t("set.pwd.update") }}
                </button>
              </div>
            </div>

            <!-- 两步验证 -->
            <div>
              <div class="mb-2 flex items-center gap-2 text-xs font-medium">
                {{ t("set.totp.title")
                }}<span
                  class="rounded px-1.5 py-0.5 text-3xs"
                  :class="
                    me.data?.value?.totpEnabled ? 'bg-ok/10 text-ok' : 'bg-surface-3 text-fg-2'
                  "
                >
                  {{ me.data?.value?.totpEnabled ? t("set.enabled") : t("set.disabled") }}
                </span>
              </div>
              <template v-if="!me.data?.value?.totpEnabled">
                <template v-if="!totpPending">
                  <button
                    class="rounded-lg border border-line px-3 py-1 text-xs disabled:opacity-50"
                    :disabled="totpBusy"
                    @click="startTotp"
                  >
                    {{ t("set.totp.generate") }}
                  </button>
                </template>
                <template v-else>
                  <p class="mb-1 text-2xs text-fg-2">{{ t("set.totp.scanHint") }}</p>
                  <code
                    class="block break-all rounded-lg bg-surface-3 px-2 py-1.5 font-mono text-2xs"
                    >{{ totpPending.otpauth }}</code
                  >
                  <p class="mt-1 text-2xs text-fg-2">
                    {{ t("set.totp.manual")
                    }}<span class="font-mono">{{ totpPending.secret }}</span>
                  </p>
                  <div class="mt-2 flex gap-2">
                    <input
                      v-model="totpCode"
                      maxlength="6"
                      inputmode="numeric"
                      :placeholder="t('set.totp.code')"
                      class="w-32 rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-xs tracking-widest"
                    />
                    <button
                      class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
                      :disabled="totpBusy"
                      @click="confirmTotp"
                    >
                      {{ t("set.totp.enable") }}
                    </button>
                  </div>
                </template>
              </template>
              <template v-else>
                <div class="flex gap-2">
                  <input
                    v-model="totpCode"
                    maxlength="6"
                    inputmode="numeric"
                    :placeholder="t('set.totp.disableCode')"
                    class="w-32 rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono text-xs tracking-widest"
                  />
                  <button
                    class="rounded-lg border border-danger/40 px-3 py-1 text-xs text-danger disabled:opacity-50"
                    :disabled="totpBusy"
                    @click="disableTotp"
                  >
                    {{ t("set.totp.disable") }}
                  </button>
                </div>
              </template>
            </div>
          </div>
        </Card>
      </div>

      <!-- 关于 -->
      <div id="s-about">
        <Card :title="t('set.about.title')" :hint="t('set.about.hint')">
          <div class="mb-3 flex items-center gap-3">
            <div
              class="grid h-9 w-9 place-items-center rounded-lg bg-gradient-to-br from-primary to-info text-sm2 font-bold text-white"
            >
              F
            </div>
            <div>
              <div class="text-sm2 font-semibold">FlinkOps Console</div>
              <div class="font-mono text-2xs text-fg-2">
                {{
                  me.data?.value ? `v0.1.0 · ${me.data.value.clusters.length} cluster` : "v0.1.0"
                }}
              </div>
            </div>
            <div class="ml-auto flex gap-2">
              <button
                class="rounded-lg border border-line px-2.5 py-1 text-2xs hover:bg-hover"
                @click="licOpen = true"
              >
                {{ t("set.about.licenses") }}
              </button>
            </div>
          </div>
          <div class="overflow-hidden rounded-lg border border-line-2">
            <table class="w-full text-2xs">
              <thead class="border-b border-line-2 bg-surface-2 text-left text-2xs text-fg-2">
                <tr>
                  <th class="px-3 py-1.5">{{ t("set.about.component") }}</th>
                  <th class="px-3 py-1.5">{{ t("set.about.version") }}</th>
                  <th class="px-3 py-1.5">{{ t("set.about.desc") }}</th>
                </tr>
              </thead>
              <tbody>
                <tr class="border-b border-line-2/60">
                  <td class="px-3 py-1.5">flink-kubernetes-operator</td>
                  <td class="px-3 py-1.5 font-mono">
                    {{ operatorLines[0]?.images?.[0]?.split(":")?.pop() ?? "—" }}
                  </td>
                  <td class="px-3 py-1.5 text-fg-2">{{ t("set.about.operator") }}</td>
                </tr>
                <tr class="border-b border-line-2/60">
                  <td class="px-3 py-1.5">apache flink</td>
                  <td class="px-3 py-1.5 font-mono">
                    {{
                      [...new Set(items.map((j) => j.flinkVersion).filter(Boolean))].join(" / ") ||
                      "—"
                    }}
                  </td>
                  <td class="px-3 py-1.5 text-fg-2">{{ t("set.about.flinkImage") }}</td>
                </tr>
                <tr class="border-b border-line-2/60">
                  <td class="px-3 py-1.5">prometheus</td>
                  <td class="px-3 py-1.5 font-mono">
                    {{ me.data?.value?.prometheus ? t("set.wired") : t("set.notWired") }}
                  </td>
                  <td class="px-3 py-1.5 text-fg-2">{{ t("set.about.prom") }}</td>
                </tr>
                <tr>
                  <td class="px-3 py-1.5">{{ t("set.about.metaStore") }}</td>
                  <td class="px-3 py-1.5 font-mono">{{ back.data?.value?.kind ?? "—" }}</td>
                  <td class="px-3 py-1.5 text-fg-2">{{ t("set.about.metaStoreNote") }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </Card>
      </div>

      <!-- 最近审计 -->
      <Card :title="t('set.audit.title')" :hint="t('set.audit.hint')">
        <div class="overflow-auto">
          <table class="w-full text-2xs">
            <tbody>
              <tr
                v-for="e in recentLogs"
                :key="e.id"
                class="border-b border-line-2/60 last:border-0"
              >
                <td class="whitespace-nowrap px-2 py-1.5 font-mono text-3xs text-fg-2">
                  {{ fmtLocal(e.ts) }}
                </td>
                <td class="px-2 py-1.5">{{ e.actor }}</td>
                <td class="px-2 py-1.5 font-mono text-2xs">{{ e.action }}</td>
                <td class="px-2 py-1.5 text-fg-1">{{ e.target }}</td>
                <td
                  class="px-2 py-1.5"
                  :class="
                    e.result === 'ok'
                      ? 'text-ok'
                      : e.result === 'denied'
                        ? 'text-warn'
                        : 'text-danger'
                  "
                >
                  {{ e.result }}
                </td>
              </tr>
              <tr v-if="!recentLogs.length">
                <td class="px-2 py-5 text-center text-fg-2">{{ t("set.audit.empty") }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </Card>
    </div>
  </div>

  <Modal v-model="licOpen" :title="t('set.lic.title')" width="640px">
    <div class="flex flex-col gap-2 text-xs leading-relaxed text-fg-1">
      <p>
        <b>FlinkOps</b>{{ t("set.lic.released") }}<span class="font-mono">Apache-2.0</span
        >{{ t("set.lic.releasedTail") }}
      </p>
      <p>{{ t("set.lic.backend") }}</p>
      <p>{{ t("set.lic.frontend") }}</p>
      <p class="text-fg-2">{{ t("set.lic.trademark") }}</p>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="licOpen = false"
      >
        {{ t("set.totp.disable") }}
      </button>
    </template>
  </Modal>
</template>
