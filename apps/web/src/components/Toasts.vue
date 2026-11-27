<script setup lang="ts">
import { dismiss, toasts } from "../lib/notify";
import { t as tr } from "../lib/i18n";
const tone: Record<string, string> = {
  info: "border-info/40 bg-info/10 text-info",
  ok: "border-ok/40 bg-ok/10 text-ok",
  warn: "border-warn/40 bg-warn/10 text-warn",
  danger: "border-danger/40 bg-danger/10 text-danger",
};
</script>

<template>
  <div class="pointer-events-none fixed bottom-4 right-4 z-50 flex w-[360px] flex-col gap-2">
    <TransitionGroup name="toast">
      <div
        v-for="t in toasts"
        :key="t.id"
        class="pointer-events-auto rounded-lg border px-3 py-2 text-xs shadow-lg backdrop-blur"
        :class="tone[t.tone]"
      >
        <div class="flex items-start gap-2">
          <span class="min-w-0 flex-1">{{ t.text }}</span>
          <button
            class="shrink-0 opacity-60 hover:opacity-100"
            :aria-label="tr('toast.dismiss')"
            :title="tr('toast.close')"
            @click="dismiss(t.id)"
          >
            ✕
          </button>
        </div>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-enter-active,
.toast-leave-active {
  transition: all 0.18s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(6px);
}
</style>
