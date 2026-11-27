<script setup lang="ts">
/** 环形图（对齐模板 UI.donut）：作业状态分布 / 槽位占用 / 存储分布。 */
import { computed } from "vue";

const props = withDefaults(
  defineProps<{
    items: { label: string; value: number; color?: string }[];
    size?: number;
    centerLabel?: string;
    centerValue?: string;
  }>(),
  { size: 132 },
);

const PALETTE = ["#2563eb", "#059669", "#d97706", "#b91c1c", "#7c3aed", "#0891b2"];
const total = computed(() => props.items.reduce((a, b) => a + Math.max(0, b.value), 0));

const circles = computed(() => {
  const r = 42;
  const c = 2 * Math.PI * r;
  let acc = 0;
  return props.items.map((it, i) => {
    const frac = total.value ? Math.max(0, it.value) / total.value : 0;
    const seg = {
      color: it.color ?? PALETTE[i % PALETTE.length],
      dash: `${(frac * c).toFixed(2)} ${c.toFixed(2)}`,
      offset: (-acc * c).toFixed(2),
      r,
    };
    acc += frac;
    return seg;
  });
});
</script>

<template>
  <div class="flex items-center gap-4">
    <svg :width="props.size" :height="props.size" viewBox="0 0 100 100" class="shrink-0">
      <circle
        cx="50"
        cy="50"
        r="42"
        fill="none"
        stroke="var(--color-surface-3)"
        stroke-width="12"
      />
      <circle
        v-for="(c, i) in circles"
        :key="i"
        cx="50"
        cy="50"
        :r="c.r"
        fill="none"
        :stroke="c.color"
        stroke-width="12"
        :stroke-dasharray="c.dash"
        :stroke-dashoffset="c.offset"
        transform="rotate(-90 50 50)"
      />
      <text
        x="50"
        y="48"
        text-anchor="middle"
        class="fill-[var(--color-fg)] font-mono"
        font-size="16"
      >
        {{ props.centerValue ?? total }}
      </text>
      <text x="50" y="62" text-anchor="middle" class="fill-[var(--color-fg-2)]" font-size="7">
        {{ props.centerLabel ?? "总计" }}
      </text>
    </svg>
    <div class="flex min-w-0 flex-col gap-1 text-2xs">
      <div v-for="(it, i) in props.items" :key="i" class="flex items-center gap-2">
        <i
          class="inline-block h-2 w-2 rounded-sm"
          :style="{ background: it.color ?? PALETTE[i % PALETTE.length] }"
        ></i>
        <span class="truncate text-fg-2">{{ it.label }}</span>
        <span class="ml-auto font-mono">{{ it.value }}</span>
      </div>
    </div>
  </div>
</template>
