<script setup lang="ts">
import { computed, ref } from "vue";
import { notify } from "../lib/notify";
import PageActions from "../components/PageActions.vue";
import { ShieldAlert, ExternalLink } from "lucide-vue-next";
import { t } from "../lib/i18n";

const p = defineProps<{ cluster: string; ns: string; name: string }>();
const nonce = ref(0);
// iframe 出错不会给任何提示，只能自己兜一层：真失败时显示"反代不可用"
const failed = ref(false);
const src = computed(() => `/proxy/${p.cluster}/${p.ns}/${p.name}/?r=${nonce.value}`);
const pageActions = [
  {
    label: "刷新",
    run: () => {
      nonce.value += 1;
      notify("已重新加载 Flink UI", "ok");
    },
  },
];
</script>

<template>
  <div class="mb-2"><PageActions :items="pageActions" /></div>
  <div
    class="mb-2 flex flex-wrap items-center gap-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-2xs"
  >
    <ShieldAlert :size="15" class="text-warn" />
    <b class="text-fg">{{ t("flinkui.readonlyTitle") }}</b>
    <span class="text-fg-2">{{ t("flinkui.readonlyBody") }}</span>
    <span class="text-fg-2">·</span>
    <span class="font-mono text-3xs text-fg-2">{{ t("flinkui.wsNote") }}</span>
    <a
      :href="src"
      target="_blank"
      class="ml-auto flex items-center gap-1 text-primary hover:underline"
    >
      {{ t("flinkui.openNewTab") }} <ExternalLink :size="12" />
    </a>
  </div>

  <div class="overflow-hidden rounded-xl border border-line-2 bg-white">
    <div v-if="failed" class="flex h-[200px] items-center justify-center text-xs text-warn">
      {{ t("flinkui.loadFailed") }}
    </div>
    <iframe
      v-else
      :key="nonce"
      :src="src"
      class="h-[calc(100vh-160px)] w-full border-0"
      title="Flink Web UI"
      sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
      @load="failed = false"
      @error="failed = true"
    />
  </div>
</template>
