<script setup lang="ts">
const props = withDefaults(defineProps<{ title: string; desc?: string }>(), { desc: "" });
</script>

<template>
  <div class="rounded-xl border border-dashed border-line bg-surface/40 px-4 py-10 text-center">
    <div class="text-sm2 font-medium text-fg-1">{{ props.title }}</div>
    <p v-if="props.desc" class="mx-auto mt-1.5 max-w-lg text-2xs leading-relaxed text-fg-2">
      {{ props.desc }}
    </p>
    <div class="mt-3 flex justify-center gap-2"><slot /></div>
  </div>
</template>
