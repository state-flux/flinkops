<script setup lang="ts">
/** 迷你趋势线（KPI 卡内嵌，对齐模板 UI.sparkline）。 */
import { computed } from "vue";

const props = withDefaults(defineProps<{ data: number[]; color?: string; height?: number }>(), {
  color: "var(--color-primary)",
  height: 30,
});

const d = computed(() => {
  const a = props.data.filter((n) => Number.isFinite(n));
  if (a.length < 2) return "";
  const max = Math.max(1, ...a);
  const min = Math.min(...a);
  const span = max - min || 1;
  return a
    .map((v, i) => {
      const x = (i / (a.length - 1)) * 120;
      const y = props.height - ((v - min) / span) * (props.height - 4) - 2;
      return `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
});
</script>

<template>
  <svg
    viewBox="0 0 120 32"
    preserveAspectRatio="none"
    class="w-full"
    :style="{ height: props.height + 'px' }"
  >
    <path
      :d="d"
      fill="none"
      :stroke="props.color"
      stroke-width="1.6"
      vector-effect="non-scaling-stroke"
    />
  </svg>
</template>
