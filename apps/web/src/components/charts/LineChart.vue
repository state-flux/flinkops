<script setup lang="ts">
/**
 * 多序列折线图（无依赖，纯 SVG）。
 *
 * 对齐模板 assets/app.js 的 UI.lineChart 能力：多 series + 图例 + 时间轴 + 阈值线 + hover 十字线。
 * 说明：
 * - 绘图区用 viewBox + preserveAspectRatio="none" 拉满宽度，因此**文字不放 SVG**，
 *   轴标签/tooltip 一律用 HTML 叠层，避免拉伸变形。
 * - 值域从 0 起（时序吞吐类指标为 0 更直观）；yFmt 负责格式化。
 */
import { computed, ref } from "vue";

export type LineSeries = { name: string; data: number[]; color?: string };

const props = withDefaults(
  defineProps<{
    series: LineSeries[];
    labels?: string[];
    height?: number;
    yFmt?: (v: number) => string;
    /** 阈值线（例如 checkpoint 超时） */
    threshold?: { value: number; label?: string; color?: string };
    empty?: string;
  }>(),
  { height: 200, empty: "没有数据" },
);

const PALETTE = ["#2563eb", "#059669", "#7c3aed", "#d97706", "#0891b2", "#db2777"];
const color = (i: number, s?: string) => s ?? PALETTE[i % PALETTE.length];

const hover = ref<number | null>(null);
const W = 1000;

const maxY = computed(() => {
  const all = props.series.flatMap((s) => s.data.filter((n) => Number.isFinite(n)));
  const t = props.threshold?.value ?? 0;
  return Math.max(1, t, ...all);
});

const n = computed(() => Math.max(...props.series.map((s) => s.data.length), 0));

const pathOf = (s: LineSeries) => {
  const d = s.data;
  if (d.length < 2) return "";
  // x 用全局最大点数对齐：每条序列按自身长度归一化会让多序列在时间轴上错位
  const span = Math.max(1, n.value - 1);
  return d
    .map((v, i) => {
      const x = (i / span) * W;
      const y = props.height - (Number(v) / maxY.value) * props.height;
      return `${i === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");
};

const ticks = computed(() => [0, 0.25, 0.5, 0.75, 1].map((r) => Math.round(maxY.value * r)));
const fmt = (v: number) => (props.yFmt ? props.yFmt(v) : String(Math.round(v)));

const axisLabels = computed(() => {
  const L = props.labels ?? [];
  if (!L.length) return [];
  if (L.length <= 4) return L;
  return [L[0], L[Math.floor(L.length / 2)], L[L.length - 1]];
});

const onMove = (e: MouseEvent) => {
  const el = e.currentTarget as HTMLElement;
  const rect = el.getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
  hover.value = Math.round(ratio * (n.value - 1));
};

const hoverX = computed(() =>
  hover.value === null || n.value < 2 ? 0 : (hover.value / (n.value - 1)) * 100,
);
</script>

<template>
  <div class="w-full">
    <div class="flex items-stretch gap-2">
      <!-- y 轴 -->
      <div
        class="flex w-[54px] shrink-0 flex-col justify-between py-[2px] text-right font-mono text-3xs text-fg-2"
      >
        <span v-for="t in [...ticks].reverse()" :key="t">{{ fmt(t) }}</span>
      </div>

      <div class="relative min-w-0 flex-1" :style="{ height: props.height + 'px' }">
        <svg
          :viewBox="`0 0 ${W} ${props.height}`"
          preserveAspectRatio="none"
          class="h-full w-full overflow-visible"
          @mousemove="onMove"
          @mouseleave="hover = null"
        >
          <line
            v-for="(t, i) in ticks"
            :key="'g' + i"
            x1="0"
            :x2="W"
            :y1="props.height - (t / maxY) * props.height"
            :y2="props.height - (t / maxY) * props.height"
            stroke="var(--color-line)"
            stroke-width="1"
            vector-effect="non-scaling-stroke"
          />
          <line
            v-if="props.threshold"
            x1="0"
            :x2="W"
            :y1="props.height - (props.threshold.value / maxY) * props.height"
            :y2="props.height - (props.threshold.value / maxY) * props.height"
            :stroke="props.threshold.color ?? 'var(--color-danger)'"
            stroke-dasharray="5 4"
            stroke-width="1"
            vector-effect="non-scaling-stroke"
          />
          <path
            v-for="(s, i) in props.series"
            :key="'s' + i"
            :d="pathOf(s)"
            fill="none"
            :stroke="color(i, s.color)"
            stroke-width="1.8"
            stroke-linejoin="round"
            vector-effect="non-scaling-stroke"
          />
          <line
            v-if="hover !== null"
            :x1="(hoverX / 100) * W"
            :x2="(hoverX / 100) * W"
            y1="0"
            :y2="props.height"
            stroke="var(--color-fg-2)"
            stroke-width="1"
            vector-effect="non-scaling-stroke"
          />
        </svg>

        <div
          v-if="hover !== null"
          class="pointer-events-none absolute top-0 z-10 min-w-[128px] rounded-lg border border-line bg-surface px-2 py-1.5 text-2xs shadow-sm"
          :style="{
            left: `calc(${hoverX}% + 8px)`,
            transform: hoverX > 70 ? 'translateX(-100%)' : 'none',
          }"
        >
          <div v-if="props.labels?.[hover]" class="mb-0.5 font-mono text-3xs text-fg-2">
            {{ props.labels[hover] }}
          </div>
          <div v-for="(s, i) in props.series" :key="'t' + i" class="flex items-center gap-1.5">
            <i
              class="inline-block h-1.5 w-1.5 rounded-full"
              :style="{ background: color(i, s.color) }"
            ></i>
            <span class="truncate text-fg-2">{{ s.name }}</span>
            <span class="ml-auto font-mono">{{ fmt(Number(s.data[hover] ?? 0)) }}</span>
          </div>
        </div>

        <div
          v-if="!props.series.length || n < 2"
          class="absolute inset-0 grid place-items-center text-xs text-fg-2"
        >
          {{ props.empty }}
        </div>
      </div>
    </div>

    <div class="mt-1 flex justify-between pl-[62px] font-mono text-3xs text-fg-2">
      <span v-for="(l, i) in axisLabels" :key="'a' + i">{{ l }}</span>
    </div>

    <div v-if="props.series.length" class="mt-2 flex flex-wrap gap-3 pl-[62px] text-2xs text-fg-2">
      <span v-for="(s, i) in props.series" :key="'l' + i" class="inline-flex items-center gap-1.5">
        <i class="inline-block h-1.5 w-3 rounded-sm" :style="{ background: color(i, s.color) }"></i
        >{{ s.name }}
      </span>
      <span v-if="props.threshold" class="inline-flex items-center gap-1.5">
        <i
          class="inline-block h-0 w-3 border-t border-dashed"
          :style="{ borderColor: props.threshold.color ?? 'var(--color-danger)' }"
        ></i>
        {{ props.threshold.label ?? "阈值" }}
      </span>
    </div>
  </div>
</template>
