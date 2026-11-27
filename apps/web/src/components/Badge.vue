<script setup lang="ts">
const props = withDefaults(defineProps<{ tone?: string; dot?: boolean }>(), { tone: "idle" });
const map: Record<string, string> = {
  ok: "bg-ok/12 text-ok border-ok/30",
  warn: "bg-warn/12 text-warn border-warn/30",
  danger: "bg-danger/12 text-danger border-danger/30",
  info: "bg-info/12 text-info border-info/30",
  idle: "bg-surface-3 text-fg-2 border-line",
};
</script>

<template>
  <span
    class="inline-flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-3xs font-medium"
    :class="map[props.tone] ?? map.idle"
  >
    <span v-if="props.dot" class="h-1.5 w-1.5 rounded-full bg-current"></span>
    <slot />
  </span>
</template>
