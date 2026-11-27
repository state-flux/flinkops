<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import Modal from "./Modal.vue";
import { confirmState, settleCancel, settleConfirm } from "../lib/confirm";

const inputValue = ref("");
const inputEl = ref<HTMLInputElement | null>(null);

watch(
  () => confirmState.open,
  (open) => {
    if (!open) return;
    inputValue.value = confirmState.opts.input?.value ?? "";
    // 弹窗打开后聚焦输入框（confirm 模式没有输入框，焦点落在确认按钮上）
    requestAnimationFrame(() => inputEl.value?.focus());
  },
);

const title = computed(() => {
  if (confirmState.opts.title) return confirmState.opts.title;
  return confirmState.mode === "prompt"
    ? "请输入"
    : confirmState.opts.danger
      ? "高危操作"
      : "请确认";
});
// 文案里允许写 **加粗**：渲染成 <strong>，而不是把星号原样丢给用户。
// （第一版"取消作业"的文案就写了 **…**，弹窗是纯文本渲染，结果屏幕上多了两个星号。）
const messageSegments = computed(() => {
  const src = confirmState.opts.message ?? "";
  const out: { text: string; bold: boolean }[] = [];
  const re = /\*\*([^*]+)\*\*/g;
  let last = 0;
  for (let m = re.exec(src); m; m = re.exec(src)) {
    if (m.index > last) out.push({ text: src.slice(last, m.index), bold: false });
    out.push({ text: m[1] ?? "", bold: true });
    last = m.index + (m[0]?.length ?? 0);
  }
  if (last < src.length) out.push({ text: src.slice(last), bold: false });
  return out.length ? out : [{ text: src, bold: false }];
});

const okText = computed(
  () => confirmState.opts.okText ?? (confirmState.mode === "prompt" ? "确定" : "确定"),
);
const cancelText = computed(() => confirmState.opts.cancelText ?? "取消");

function onOk() {
  settleConfirm(confirmState.mode === "prompt" ? inputValue.value : true);
}
function onCancel() {
  settleCancel();
}
function onKey(e: KeyboardEvent) {
  if (!confirmState.open) return;
  if (e.key === "Enter" && confirmState.mode === "prompt") {
    e.preventDefault();
    onOk();
  }
}

// 全局键盘：Enter = 确定（多行输入框里除外），Esc = 取消。
// 弹窗是 persistent 的（危险操作不许点背板误关），所以 Esc 在这里显式兜底。
function onWindowKey(e: KeyboardEvent) {
  if (!confirmState.open) return;
  const tag = (e.target as HTMLElement | null)?.tagName;
  if (e.key === "Enter" && tag !== "TEXTAREA") {
    e.preventDefault();
    onOk();
  } else if (e.key === "Escape") {
    e.preventDefault();
    onCancel();
  }
}
onMounted(() => window.addEventListener("keydown", onWindowKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onWindowKey));
</script>

<template>
  <Modal
    :model-value="confirmState.open"
    :title="title"
    width="520px"
    :persistent="!!confirmState.opts.danger"
    @update:model-value="
      (v: boolean) => {
        if (!v) onCancel();
      }
    "
  >
    <p class="whitespace-pre-line text-xs leading-relaxed text-fg">
      <template v-for="(seg, i) in messageSegments" :key="i">
        <strong v-if="seg.bold" class="font-semibold">{{ seg.text }}</strong>
        <template v-else>{{ seg.text }}</template>
      </template>
    </p>
    <p v-if="confirmState.opts.detail" class="mt-2 text-2xs leading-relaxed text-fg-2">
      {{ confirmState.opts.detail }}
    </p>
    <label v-if="confirmState.mode === 'prompt'" class="mt-3 flex flex-col gap-1 text-xs">
      <span v-if="confirmState.opts.input?.label" class="text-fg-2">{{
        confirmState.opts.input.label
      }}</span>
      <textarea
        v-if="confirmState.opts.input?.multiline"
        v-model="inputValue"
        rows="4"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        :placeholder="confirmState.opts.input?.placeholder"
        @keydown="onKey"
      />
      <input
        v-else
        ref="inputEl"
        v-model="inputValue"
        class="rounded-lg border border-line bg-surface-2 px-2 py-1.5 font-mono"
        :placeholder="confirmState.opts.input?.placeholder"
        @keydown="onKey"
      />
    </label>
    <template #footer>
      <button
        class="rounded-lg border border-line px-3 py-1 text-xs hover:bg-hover"
        @click="onCancel"
      >
        {{ cancelText }}
      </button>
      <button
        class="rounded-lg px-3 py-1 text-xs font-medium text-white"
        :class="
          confirmState.opts.danger ? 'bg-danger hover:opacity-90' : 'bg-primary hover:opacity-90'
        "
        @click="onOk"
      >
        {{ okText }}
      </button>
    </template>
  </Modal>
</template>
