<script setup lang="ts">
import { todo } from "../lib/notify";

/**
 * 设计稿里的操作按钮组。
 * 传 handlers 里的动作会真调用，其余点击给出"未实现"提示 ——
 * 按钮必须在，能力可以缺，但不能假装这个功能不存在。
 */
const props = defineProps<{
  items: { label: string; run?: () => void; primary?: boolean }[];
}>();
</script>

<template>
  <div class="flex flex-wrap items-center gap-2">
    <button
      v-for="it in props.items"
      :key="it.label"
      class="rounded-lg border px-2.5 py-1 text-xs transition disabled:opacity-50"
      :class="
        it.primary
          ? 'border-primary bg-primary text-white hover:opacity-90'
          : 'border-line bg-surface-2 text-fg-1 hover:border-primary/50 hover:text-fg'
      "
      @click="it.run ? it.run() : todo(it.label)"
    >
      {{ it.label }}
    </button>
  </div>
</template>
