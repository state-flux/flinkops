<script setup lang="ts">
/** 迷你柱状（对齐模板 UI.barChart，用于"近 24 小时出向峰值"）。 */
import { computed } from "vue";

const props = withDefaults(defineProps<{ data: number[]; height?: number; color?: string }>(), {
  height: 54,
  color: "rgba(37,99,235,.55)",
});

const max = computed(() => Math.max(1, ...props.data));
</script>

<template>
  <div class="flex items-end gap-[2px]" :style="{ height: props.height + 'px' }">
    <div
      v-for="(v, i) in props.data"
      :key="i"
      class="flex-1 rounded-t-[2px]"
      :style="{ height: Math.max(2, (v / max) * props.height) + 'px', background: props.color }"
      :title="String(v)"
    ></div>
  </div>
</template>
