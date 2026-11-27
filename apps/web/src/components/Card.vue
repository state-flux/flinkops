<script setup lang="ts">
const props = withDefaults(defineProps<{ title?: string; hint?: string; pad?: boolean }>(), {
  pad: true,
});
</script>

<template>
  <section class="rounded-xl border border-line-2 bg-surface">
    <header
      v-if="props.title || $slots.actions"
      class="flex items-center gap-2 border-b border-line-2/70 px-4 py-2.5"
    >
      <h2 class="text-sm2 font-semibold">{{ props.title }}</h2>
      <span v-if="props.hint" class="text-2xs text-fg-2">{{ props.hint }}</span>
      <div class="ml-auto flex items-center gap-2"><slot name="actions" /></div>
    </header>
    <div :class="props.pad ? 'p-4' : ''"><slot /></div>
  </section>
</template>
