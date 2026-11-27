<script setup lang="ts">
const props = withDefaults(
  defineProps<{ label: string; value: string | number; foot?: string; tone?: string }>(),
  { tone: "fg" },
);
const toneMap: Record<string, string> = {
  fg: "text-fg",
  ok: "text-ok",
  warn: "text-warn",
  danger: "text-danger",
  primary: "text-primary",
};
</script>

<template>
  <div class="rounded-xl border border-line-2 bg-surface px-4 py-3">
    <div class="text-2xs text-fg-2">{{ props.label }}</div>
    <div
      class="mt-1 font-mono text-[22px] leading-tight"
      :class="toneMap[props.tone] ?? toneMap.fg"
    >
      {{ props.value }}
    </div>
    <div v-if="props.foot" class="mt-0.5 text-2xs text-fg-2">{{ props.foot }}</div>
  </div>
</template>
