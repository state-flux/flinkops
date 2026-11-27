<script setup lang="ts">
const props = defineProps<{
  title: string;
  sub?: string;
  badge?: string;
  tone?: string;
  hideCluster?: boolean;
}>();
const toneMap: Record<string, string> = {
  ok: "bg-ok/12 text-ok",
  warn: "bg-warn/12 text-warn",
  danger: "bg-danger/12 text-danger",
  info: "bg-info/12 text-info",
  idle: "bg-surface-3 text-fg-2",
};
</script>

<template>
  <!-- 集群切换已上移到 App 的固定顶栏（`hideCluster` 仅保留兼容，不再渲染切换器） -->
  <div class="mb-4 flex flex-wrap items-end gap-3">
    <div class="min-w-0">
      <h1 class="text-[19px] font-semibold tracking-tight">
        {{ props.title }}
        <span
          v-if="props.badge"
          class="ml-2 rounded-full px-2 py-0.5 align-middle text-2xs"
          :class="toneMap[props.tone ?? 'idle']"
          >{{ props.badge }}</span
        >
      </h1>
      <p v-if="props.sub" class="mt-0.5 text-xs text-fg-2">{{ props.sub }}</p>
    </div>
    <div class="ml-auto flex flex-wrap items-center gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>
