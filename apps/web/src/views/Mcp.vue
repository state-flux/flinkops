<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, onMounted, onUnmounted, ref } from "vue";
import { useQuery, useQueryClient } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import { mcp } from "../api/client";
import { notify } from "../lib/notify";
import { fmtLocal } from "../lib/time";
import { t } from "../lib/i18n";

interface TokenRow {
  id: string;
  name: string;
  scopes: string[];
  allowNamespaces: string[];
  allowJobs: string[];
  qps: number;
  expiresAt?: string | null;
  createdAt: string;
  lastUsedAt?: string | null;
  revokedAt?: string | null;
}
interface ApprovalRow {
  id: string;
  tool: string;
  target: string;
  argsHash: string;
  actor: string;
  status: string;
  createdAt: string;
  decidedAt?: string | null;
  decidedBy?: string | null;
}

const qc = useQueryClient();

const tokens = useQuery({
  queryKey: ["mcp", "tokens"],
  queryFn: async () => (await mcp.tokens()) as { items: TokenRow[] },
  retry: false,
});
const approvals = useQuery({
  queryKey: ["mcp", "approvals"],
  queryFn: async () => (await mcp.approvals()) as { items: ApprovalRow[] },
  refetchInterval: 15_000,
  retry: false,
});

const tokenRows = computed(() => tokens.data?.value?.items ?? []);
const approvalRows = computed(() => approvals.data?.value?.items ?? []);
const pendingRows = computed(() => approvalRows.value.filter((a) => a.status === "pending"));

// 创建表单
const form = ref({
  name: "",
  scopes: ["read"] as string[],
  allowNs: "flink",
  allowJobs: "",
  qps: 5,
  days: 30,
});
const created = ref<{ id: string; token: string } | null>(null);
const busy = ref(false);

const scopeOptions = ["read", "operate", "admin"];

function toggleScope(s: string) {
  const set = new Set(form.value.scopes);
  if (set.has(s)) set.delete(s);
  else set.add(s);
  form.value.scopes = [...set];
}

async function create() {
  if (!form.value.name.trim()) {
    notify(t("mcp.namePrompt"), "warn");
    return;
  }
  busy.value = true;
  try {
    const split = (s: string) =>
      s
        .split(/[,\s]+/)
        .map((x) => x.trim())
        .filter(Boolean);
    const r = await mcp.createToken({
      name: form.value.name.trim(),
      scopes: form.value.scopes,
      allowNamespaces: split(form.value.allowNs),
      allowJobs: split(form.value.allowJobs),
      qps: form.value.qps,
      expiresInDays: form.value.days,
    });
    created.value = { id: r.id, token: r.token };
    form.value.name = "";
    notify(t("mcp.created"), "ok", 6000);
    await qc.invalidateQueries({ queryKey: ["mcp", "tokens"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  } finally {
    busy.value = false;
  }
}

function copyToken() {
  if (created.value) void navigator.clipboard?.writeText(created.value.token);
}

async function revoke(id: string) {
  if (
    !(await confirmDialog({
      message: t("mcp.revokeConfirm"),
      danger: true,
    }))
  )
    return;
  try {
    await mcp.revokeToken(id);
    notify(t("mcp.revoked"), "ok");
    await qc.invalidateQueries({ queryKey: ["mcp", "tokens"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

async function decide(id: string, decision: "approve" | "reject") {
  try {
    const r = await mcp.decide(id, decision);
    if (decision === "approve" && r.approvalToken) {
      await navigator.clipboard?.writeText(r.approvalToken).catch(() => {});
      notify(t("mcp.approved", { token: r.approvalToken.slice(0, 14) }), "ok", 9000);
    } else {
      notify(t("mcp.rejected"), "ok");
    }
    await qc.invalidateQueries({ queryKey: ["mcp", "approvals"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

// SSE：MCP 一落新审批就推过来，不用等 15s 轮询
const sseState = ref<"connecting" | "live" | "offline">("connecting");
let es: EventSource | null = null;

onMounted(() => {
  es = new EventSource("/api/approvals/stream");
  es.addEventListener("hello", () => {
    sseState.value = "live";
  });
  es.addEventListener("approval", (ev) => {
    let payload: {
      type?: string;
      tool?: string;
      target?: string;
      actor?: string;
      decision?: string;
    } = {};
    try {
      payload = JSON.parse((ev as MessageEvent).data);
    } catch {
      /* 忽略坏包 */
    }
    if (payload.type === "pending") {
      notify(
        t("mcp.newApproval", {
          tool: (payload.tool ?? "").replace("flinkops_", ""),
          target: payload.target ?? "",
        }),
        "warn",
        10000,
      );
      void approvals.refetch();
    } else if (payload.type === "decided") {
      void approvals.refetch();
    }
  });
  es.onerror = () => {
    // EventSource 会自己重连，这里只更新状态
    sseState.value = "offline";
  };
});

onUnmounted(() => {
  es?.close();
  es = null;
});

const pageActions = computed(() => [
  {
    label: t("mcp.refresh"),
    run: () => {
      void tokens.refetch();
      void approvals.refetch();
      notify(t("mcp.refreshed"), "ok");
    },
  },
  {
    label: t("mcp.copyConfig"),
    run: () => {
      const snippet = `{
  "mcpServers": {
    "flinkops": {
      "command": "flinkops-mcp",
      "args": ["--stdio"],
      "env": {
        "FLINKOPS_URL": "${location.origin}",
        "FLINKOPS_MCP_TOKEN": "fko_pat_<id>_<secret>"
      }
    }
  }
}`;
      void navigator.clipboard?.writeText(snippet);
      notify(t("mcp.configCopied"), "ok");
    },
  },
]);
</script>

<template>
  <PageHead
    :title="t('page.mcp')"
    :sub="t('mcp.sub')"
    :badge="
      pendingRows.length
        ? t('mcp.badgePending', { n: pendingRows.length })
        : t('mcp.badgeNoPending')
    "
    :tone="pendingRows.length ? 'warn' : 'ok'"
  >
    <template #actions><PageActions :items="pageActions" /></template>
  </PageHead>

  <div class="mb-3 grid gap-3 xl:grid-cols-[1fr_1.1fr]">
    <Card :title="t('mcp.createToken')" :hint="t('mcp.createHint')">
      <div class="flex flex-col gap-2 text-xs">
        <label class="flex flex-col gap-1">
          <span class="text-fg-2">{{ t("mcp.name") }}</span>
          <input
            v-model="form.name"
            class="rounded-lg border border-line bg-surface-2 px-2 py-1"
            placeholder="claude-desktop / cursor"
          />
        </label>
        <div class="flex flex-col gap-1">
          <span class="text-fg-2">Scope</span>
          <div class="flex gap-1">
            <button
              v-for="s in scopeOptions"
              :key="s"
              class="rounded-full border px-2.5 py-0.5 text-2xs"
              :class="
                form.scopes.includes(s)
                  ? 'border-primary/40 bg-primary/10 text-primary'
                  : 'border-line text-fg-2'
              "
              @click="toggleScope(s)"
            >
              {{ s }}
            </button>
          </div>
          <p class="text-3xs text-fg-2">{{ t("mcp.roleHint") }}</p>
        </div>
        <div class="grid gap-2 sm:grid-cols-2">
          <label class="flex flex-col gap-1">
            <span class="text-fg-2">{{ t("mcp.allowNs") }}</span>
            <input
              v-model="form.allowNs"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              placeholder="flink, flink-*"
            />
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-fg-2">{{ t("mcp.allowJobs") }}</span>
            <input
              v-model="form.allowJobs"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
              placeholder="flink/ant-*, flink/api-*"
            />
          </label>
        </div>
        <div class="grid gap-2 sm:grid-cols-2">
          <label class="flex flex-col gap-1">
            <span class="text-fg-2">QPS</span>
            <input
              v-model.number="form.qps"
              type="number"
              min="1"
              max="100"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1"
            />
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-fg-2">{{ t("mcp.ttl") }}</span>
            <input
              v-model.number="form.days"
              type="number"
              min="1"
              class="rounded-lg border border-line bg-surface-2 px-2 py-1"
            />
          </label>
        </div>
        <button
          class="mt-1 rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50"
          :disabled="busy"
          @click="create"
        >
          {{ busy ? t("mcp.creating") : t("mcp.createToken") }}
        </button>

        <div v-if="created" class="mt-1 rounded-lg border border-ok/30 bg-ok/5 p-2">
          <div class="mb-1 text-2xs font-medium text-ok">{{ t("mcp.secretOnce") }}</div>
          <code class="block break-all rounded bg-surface-2 px-2 py-1 font-mono text-2xs">{{
            created.token
          }}</code>
          <button class="mt-1 text-2xs text-primary hover:underline" @click="copyToken">
            {{ t("mcp.copy") }}
          </button>
        </div>
      </div>
    </Card>

    <Card
      :title="t('mcp.pendingCard')"
      :hint="
        t('mcp.pendingHint', {
          n: pendingRows.length,
          state:
            sseState === 'live'
              ? t('mcp.live')
              : sseState === 'offline'
                ? t('mcp.offline')
                : t('mcp.connecting'),
        })
      "
      :pad="false"
    >
      <div class="overflow-auto">
        <table class="w-full text-2xs">
          <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
            <tr>
              <th class="px-3 py-2">{{ t("mcp.col.tool") }}</th>
              <th class="px-3 py-2">{{ t("mcp.col.target") }}</th>
              <th class="px-3 py-2">{{ t("mcp.col.actor") }}</th>
              <th class="px-3 py-2">{{ t("mcp.col.time") }}</th>
              <th class="px-3 py-2">{{ t("mcp.col.actions") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="a in pendingRows"
              :key="a.id"
              class="border-b border-line-2/60 last:border-0"
            >
              <td class="px-3 py-1.5 font-mono">{{ a.tool.replace("flinkops_", "") }}</td>
              <td class="px-3 py-1.5 font-mono">{{ a.target }}</td>
              <td class="px-3 py-1.5 text-3xs text-fg-2">{{ a.actor }}</td>
              <td class="px-3 py-1.5 text-3xs text-fg-2">{{ a.createdAt.slice(11, 19) }}</td>
              <td class="px-3 py-1.5">
                <button class="mr-2 text-ok hover:underline" @click="decide(a.id, 'approve')">
                  {{ t("mcp.approve") }}
                </button>
                <button class="text-danger hover:underline" @click="decide(a.id, 'reject')">
                  {{ t("mcp.reject") }}
                </button>
              </td>
            </tr>
            <tr v-if="!pendingRows.length">
              <td colspan="5" class="px-3 py-6 text-center text-xs text-fg-2">
                {{ t("mcp.pendingEmpty") }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Card>
  </div>

  <Card
    :title="t('mcp.tokensCard')"
    :hint="t('mcp.tokenCount', { n: tokenRows.length })"
    :pad="false"
    class="mb-3"
  >
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("mcp.col.name") }}</th>
            <th class="px-3 py-2">Scope</th>
            <th class="px-3 py-2">{{ t("mcp.col.scope") }}</th>
            <th class="px-3 py-2">QPS</th>
            <th class="px-3 py-2">{{ t("mcp.col.lastUsed") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.state") }}</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="tok in tokenRows"
            :key="tok.id"
            class="border-b border-line-2/60 last:border-0"
          >
            <td class="px-3 py-1.5">
              <div class="font-medium">{{ t.name }}</div>
              <div class="font-mono text-3xs text-fg-2">{{ tok.id }}</div>
            </td>
            <td class="px-3 py-1.5 font-mono text-3xs">{{ tok.scopes.join(",") }}</td>
            <td class="px-3 py-1.5 font-mono text-3xs text-fg-2">
              ns: {{ tok.allowNamespaces.join(",") || "—" }}<br />
              job: {{ tok.allowJobs.join(",") || t("mcp.allJobs") }}
            </td>
            <td class="px-3 py-1.5 font-mono">{{ tok.qps }}</td>
            <td class="px-3 py-1.5 text-3xs text-fg-2">
              {{ tok.lastUsedAt ? fmtLocal(tok.lastUsedAt) : t("mcp.never") }}
            </td>
            <td class="px-3 py-1.5">
              <Badge :tone="tok.revokedAt ? 'idle' : 'ok'">{{
                tok.revokedAt ? t("mcp.revokedState") : t("mcp.valid")
              }}</Badge>
            </td>
            <td class="px-3 py-1.5 text-right">
              <button
                v-if="!tok.revokedAt"
                class="text-danger hover:underline"
                @click="revoke(tok.id)"
              >
                {{ t("mcp.revoke") }}
              </button>
            </td>
          </tr>
          <tr v-if="!tokenRows.length">
            <td colspan="7" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("mcp.tokensEmpty") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>

  <Card :title="t('mcp.approvalsCard')" :hint="`${approvalRows.length} 条`" :pad="false">
    <div class="overflow-auto">
      <table class="w-full text-2xs">
        <thead class="border-b border-line-2 text-left text-2xs text-fg-2">
          <tr>
            <th class="px-3 py-2">{{ t("mcp.col.tool") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.target") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.actor") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.state") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.approver") }}</th>
            <th class="px-3 py-2">{{ t("mcp.col.time") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="a in approvalRows" :key="a.id" class="border-b border-line-2/60 last:border-0">
            <td class="px-3 py-1.5 font-mono">{{ a.tool.replace("flinkops_", "") }}</td>
            <td class="px-3 py-1.5 font-mono">{{ a.target }}</td>
            <td class="px-3 py-1.5 text-3xs text-fg-2">{{ a.actor }}</td>
            <td class="px-3 py-1.5">
              <Badge
                :tone="
                  a.status === 'pending'
                    ? 'warn'
                    : a.status === 'approved'
                      ? 'ok'
                      : a.status === 'used'
                        ? 'info'
                        : 'idle'
                "
                >{{ a.status }}</Badge
              >
            </td>
            <td class="px-3 py-1.5 text-3xs text-fg-2">{{ a.decidedBy ?? "—" }}</td>
            <td class="px-3 py-1.5 text-3xs text-fg-2">
              {{ fmtLocal(a.createdAt) }}
            </td>
          </tr>
          <tr v-if="!approvalRows.length">
            <td colspan="6" class="px-3 py-6 text-center text-xs text-fg-2">
              {{ t("mcp.approvalsEmpty") }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </Card>
</template>
