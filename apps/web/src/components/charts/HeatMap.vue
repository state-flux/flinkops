<script setup lang="ts">
import { computed } from "vue";
import { t } from "../../lib/i18n";
/** 背压矩阵（对齐模板 UI.heatmap）：行=作业，列=时间桶，值域 0..1。 */
const props = withDefaults(
  defineProps<{
    rows: { label: string; values: number[] }[];
    cols?: number;
    timeHint?: { start: string; end: string };
  }>(),
  { cols: 16 },
);
const hint = computed(() => props.timeHint ?? { start: "6h 前", end: "现在" });

const colorOf = (v: number) => {
  const x = Math.min(1, Math.max(0, v));
  if (x < 0.35) return `rgba(5,150,105,${0.16 + x * 0.9})`; // ok 绿
  if (x < 0.6) return `rgba(217,119,6,${0.35 + (x - 0.35) * 1.2})`; // warn 琥珀
  return `rgba(185,28,28,${0.4 + (x - 0.6) * 1.4})`; // danger 红
};
</script>

<template>
  <div class="flex flex-col gap-1">
    <div v-for="(r, ri) in props.rows" :key="ri" class="flex items-center gap-2">
      <span class="w-[118px] shrink-0 truncate font-mono text-3xs text-fg-2" :title="r.label">{{
        r.label
      }}</span>
      <div class="flex min-w-0 flex-1 gap-[3px]">
        <div
          v-for="(v, ci) in r.values"
          :key="ci"
          class="h-[13px] flex-1 rounded-[2px]"
          :style="{ background: colorOf(v) }"
          :title="`${r.label} · ${Math.round(v * 100)}%`"
        ></div>
      </div>
    </div>
    <div class="mt-1 flex items-center gap-2">
      <span class="w-[118px] shrink-0"></span>
      <div class="flex flex-1 justify-between font-mono text-3xs text-fg-2">
        <span>{{ hint.start }}</span><span>{{ hint.end }}</span>
      </div>
    </div>
    <div class="mt-1 flex items-center gap-2 pl-[118px] text-3xs text-fg-2">
      <span>{{ t("heatmap.low") }}</span>
      <div class="flex h-[8px] flex-1 overflow-hidden rounded">
        <i
          v-for="i in 5"
          :key="i"
          class="h-full flex-1"
          :style="{ background: colorOf((i - 1) / 5 + 0.08) }"
        ></i>
      </div>
      <span>{{ t("heatmap.high") }}</span>
    </div>
  </div>
</template>
