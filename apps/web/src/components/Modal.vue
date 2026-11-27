<script setup lang="ts">
import { onBeforeUnmount, onMounted } from "vue";
import { t } from "../lib/i18n";

const props = withDefaults(
  defineProps<{
    modelValue: boolean;
    title: string;
    sub?: string;
    width?: string;
    /** 破坏性确认：ESC / 点背板不关闭，只能显式点按钮 */
    persistent?: boolean;
  }>(),
  { width: "560px", persistent: false },
);

const emit = defineEmits<{ "update:modelValue": [boolean] }>();
const close = () => emit("update:modelValue", false);

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape" && !props.persistent) close();
}
onMounted(() => document.addEventListener("keydown", onKey));
onBeforeUnmount(() => document.removeEventListener("keydown", onKey));
</script>

<template>
  <Teleport to="body">
    <div
      v-if="props.modelValue"
      class="fixed inset-0 z-50 grid place-items-center bg-black/60 p-4"
      @click.self="props.persistent ? undefined : close()"
    >
      <div
        class="max-h-[86vh] w-full overflow-hidden rounded-xl border border-line-2 bg-surface shadow-2xl"
        :style="{ maxWidth: props.width }"
      >
        <header class="flex items-start gap-3 border-b border-line-2 px-4 py-3">
          <div class="min-w-0">
            <h3 class="text-[14px] font-semibold">{{ props.title }}</h3>
            <p v-if="props.sub" class="mt-0.5 text-2xs text-fg-2">{{ props.sub }}</p>
          </div>
          <button
            class="ml-auto shrink-0 text-fg-2 hover:text-fg"
            :aria-label="t('modal.close')"
            :title="t('modal.closeTitle')"
            @click="close"
          >
            ✕
          </button>
        </header>
        <div class="max-h-[64vh] overflow-auto px-4 py-3"><slot /></div>
        <footer
          v-if="$slots.footer"
          class="flex items-center justify-end gap-2 border-t border-line-2 px-4 py-2.5"
        >
          <slot name="footer" />
        </footer>
      </div>
    </div>
  </Teleport>
</template>
