<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, ref } from "vue";
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import PageHead from "../components/PageHead.vue";
import PageActions from "../components/PageActions.vue";
import Card from "../components/Card.vue";
import Badge from "../components/Badge.vue";
import EmptyState from "../components/EmptyState.vue";
import Tabs from "../components/Tabs.vue";
import Modal from "../components/Modal.vue";
import DataTable from "../components/DataTable.vue";
import { auth, request } from "../api/client";
import { useWritable } from "../lib/session";
import { notify } from "../lib/notify";
import { t } from "../lib/i18n";

const tab = ref("u-list");
const qc = useQueryClient();
const { readonly } = useWritable();
const me = useQuery({ queryKey: ["me"], queryFn: auth.me });
const info = computed(
  () =>
    me.data?.value as
      | {
          user?: string;
          role?: string;
          allowNs?: string[];
          readonly?: boolean;
          clusters?: string[];
        }
      | undefined,
);

type UserRow = {
  username: string;
  role: string;
  allowNs: string[];
  disabled: boolean;
  createdAt?: string;
};

const users = useQuery({
  queryKey: ["users"],
  queryFn: () => request<{ items: UserRow[] }>("/api/users"),
  retry: false,
  enabled: computed(() => info.value?.role === "admin"),
});

const columns = computed(() => [
  { key: "username", label: t("users.col.username") },
  { key: "role", label: t("users.col.role") },
  { key: "allowNs", label: t("users.col.ns") },
  { key: "createdAt", label: t("users.col.createdAt") },
  { key: "actions", label: t("users.col.actions"), sortable: false },
]);

const open = ref(false);
const form = ref({ username: "", password: "", role: "viewer", allowNs: "" });

function openCreate() {
  form.value = { username: "", password: "", role: "viewer", allowNs: "" };
  open.value = true;
}

const saveMut = useMutation({
  mutationFn: () =>
    request<{ ok: boolean }>("/api/users", {
      method: "POST",
      body: JSON.stringify({
        username: form.value.username.trim(),
        password: form.value.password || undefined,
        role: form.value.role,
        allowNs: form.value.allowNs
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
      }),
    }),
  onSuccess: async () => {
    notify(t("users.saved"), "ok");
    open.value = false;
    await qc.invalidateQueries({ queryKey: ["users"] });
  },
  onError: (e) => notify((e as Error).message, "danger"),
});

async function remove(username: string) {
  if (
    !(await confirmDialog({ message: t("users.deleteConfirm", { name: username }), danger: true }))
  )
    return;
  try {
    await request(`/api/users/${username}`, { method: "DELETE" });
    notify(t("users.deleted", { name: username }), "ok");
    await qc.invalidateQueries({ queryKey: ["users"] });
  } catch (e) {
    notify((e as Error).message, "danger");
  }
}

const isAdmin = computed(() => info.value?.role === "admin");
</script>

<template>
  <PageHead :title="t('page.users')" :sub="isAdmin ? t('users.sub') : t('users.notAdmin')">
    <template #actions>
      <PageActions :items="[{ label: t('users.newAccount'), run: openCreate }]" />
    </template>
  </PageHead>
  <Tabs
    v-model="tab"
    :tabs="[
      { key: 'u-list', label: t('users.tab.members') },
      { key: 'u-role', label: t('users.tab.roles') },
      { key: 'u-sa', label: 'ServiceAccount' },
    ]"
  />

  <template v-if="tab === 'u-list'">
    <Card :title="t('users.currentSession')" :hint="info?.readonly ? '平台只读模式' : '可写'">
      <div class="flex items-center gap-3 text-xs">
        <span
          class="grid h-8 w-8 place-items-center rounded-full bg-gradient-to-br from-primary to-info text-2xs font-bold text-white"
        >
          {{ (info?.user ?? "?").slice(0, 2).toUpperCase() }}
        </span>
        <div>
          <div class="font-medium">{{ info?.user ?? "—" }}</div>
          <div class="font-mono text-3xs text-fg-2">
            {{ t("users.roleLabel") }} {{ info?.role ?? "—" }} {{ t("users.nsPrefix") }}
            {{ (info?.allowNs ?? []).join(", ") || t("users.all") }}
          </div>
        </div>
        <Badge class="ml-auto" :tone="info?.readonly ? 'warn' : 'ok'">{{
          info?.readonly ? "readonly" : "read-write"
        }}</Badge>
      </div>
    </Card>

    <Card
      v-if="isAdmin"
      :title="t('users.username')"
      :hint="t('users.count', { n: users.data?.value?.items?.length ?? 0 })"
      :pad="false"
    >
      <DataTable
        :columns="columns"
        :rows="(users.data?.value?.items ?? []) as unknown as Record<string, unknown>[]"
        row-key="username"
        :empty="t('users.empty')"
      >
        <template #cell-allowNs="{ row }">
          <span class="font-mono text-3xs text-fg-2">{{
            ((row as UserRow).allowNs ?? []).join(", ") || t("users.all")
          }}</span>
        </template>
        <template #cell-role="{ row }">
          <Badge
            :tone="
              (row as UserRow).role === 'admin'
                ? 'danger'
                : (row as UserRow).role === 'operator'
                  ? 'warn'
                  : 'idle'
            "
          >
            {{ (row as UserRow).role }}
          </Badge>
        </template>
        <template #cell-createdAt="{ row }">
          <span class="font-mono text-3xs text-fg-2">{{ (row as UserRow).createdAt ?? "—" }}</span>
        </template>
        <template #cell-actions="{ row }">
          <button
            :disabled="readonly || (row as UserRow).username === info?.user"
            class="text-2xs text-danger hover:underline disabled:opacity-40"
            @click="remove((row as UserRow).username)"
          >
            {{ t("users.delete") }}
          </button>
        </template>
      </DataTable>
    </Card>
    <EmptyState v-else :title="t('users.adminOnly')" :desc="t('users.adminOnlyDesc')" />
  </template>

  <template v-else-if="tab === 'u-role'">
    <Card :title="t('users.rolesCard')">
      <ul class="list-disc pl-5 text-xs leading-relaxed text-fg-1">
        <li><b>admin</b>{{ t("users.roleAdminDesc") }}</li>
        <li>
          <b>operator</b>{{ t("users.roleOperatorDescPrefix") }}
          <code class="font-mono">allowNs</code>{{ t("users.roleOperatorDescTail") }}
        </li>
        <li><b>viewer</b>{{ t("users.roleViewerDesc") }}</li>
      </ul>
      <p class="mt-2 text-2xs text-fg-2">
        {{ t("users.platformReadonly") }}<code class="font-mono">FLINKOPS_READONLY=true</code
        >{{ t("users.platformReadonlyDesc") }}
      </p>
    </Card>
  </template>

  <template v-else>
    <EmptyState :title="t('users.saCard')" :desc="t('users.saDesc')" />
  </template>

  <Modal v-model="open" :title="t('users.upsertTitle')" :sub="t('users.upsertSub')">
    <div class="grid gap-3 text-xs">
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("users.accountsCard") }}</span>
        <input
          v-model="form.username"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        />
      </label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("users.password") }}</span>
        <input
          v-model="form.password"
          type="password"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        />
      </label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("users.col.role") }}</span>
        <select v-model="form.role" class="rounded-lg border border-line bg-surface-2 px-2 py-1.5">
          <option value="viewer">{{ t("users.roleViewer") }}</option>
          <option value="operator">{{ t("users.roleOperator") }}</option>
          <option value="admin">{{ t("users.roleAdmin") }}</option>
        </select>
      </label>
      <label class="flex flex-col gap-1"
        ><span class="text-fg-2">{{ t("users.allowNs") }}</span>
        <input
          v-model="form.allowNs"
          placeholder="flink, team-a"
          class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        />
      </label>
    </div>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="open = false"
      >
        {{ t("users.cancel") }}
      </button>
      <button
        :disabled="readonly || saveMut.isPending.value"
        class="rounded-lg bg-primary px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
        @click="saveMut.mutate()"
      >
        {{ t("users.save") }}
      </button>
    </template>
  </Modal>
</template>
