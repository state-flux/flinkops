<script setup lang="ts">
export type Tab = { key: string; label: string; badge?: string | number };

const props = defineProps<{ tabs: Tab[]; modelValue: string }>();
const emit = defineEmits<{ "update:modelValue": [string] }>();
</script>

<template>
  <div class="mb-3 flex flex-wrap items-center gap-1 border-b border-line-2">
    <button
      v-for="t in props.tabs"
      :key="t.key"
      class="relative -mb-px border-b-2 px-3 py-2 text-xs transition"
      :class="
        props.modelValue === t.key
          ? 'border-primary text-fg'
          : 'border-transparent text-fg-2 hover:text-fg-1'
      "
      @click="emit('update:modelValue', t.key)"
    >
      {{ t.label }}
      <span
        v-if="t.badge !== undefined && t.badge !== ''"
        class="ml-1 rounded bg-surface-3 px-1 py-0.5 text-3xs text-fg-2"
        >{{ t.badge }}</span
      >
    </button>
  </div>
</template>
