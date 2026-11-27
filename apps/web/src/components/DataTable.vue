<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { t } from "../lib/i18n";

export type Column = {
  key: string;
  label: string;
  align?: "left" | "right";
  width?: string;
  /** 默认 true；设为 false 关闭该列排序 */
  sortable?: boolean;
};

const props = withDefaults(
  defineProps<{
    columns: Column[];
    rows: Record<string, unknown>[];
    empty?: string;
    /** 加载失败时的错误文案；有值就渲染错误行，而不是"没有匹配的数据" */
    error?: string;
    rowKey?: string;
    /** 0 = 不分页 */
    pageSize?: number;
    /** 开启行选择（需配合 rowKey） */
    selectable?: boolean;
    /** 受控排序：配合 v-model:sort / v-model:dir 把状态写进 URL */
    sort?: string;
    dir?: "asc" | "desc";
    /** 受控页码 */
    page?: number;
    /**
     * 外部受控的选择集合。传了就以外部为准（`v-model:selected` 单向数据源），
     * 否则内部自管——两套状态并存会出现"外部全选、表格勾选框还是空的"。
     */
    selected?: string[];
  }>(),
  { empty: "没有数据", rowKey: "", pageSize: 0, selectable: false, selected: undefined },
);

const emit = defineEmits<{
  (e: "update:selected", v: string[]): void;
  (e: "update:sort", v: string): void;
  (e: "update:dir", v: "asc" | "desc"): void;
  (e: "update:page", v: number): void;
}>();
const inner = ref<Set<string>>(new Set());
const controlled = computed(() => Array.isArray(props.selected));
const selectedSet = computed(() =>
  controlled.value ? new Set(props.selected as string[]) : inner.value,
);
function commit(next: Set<string>) {
  if (!controlled.value) inner.value = next;
  emit("update:selected", [...next]);
}

function keyOf(r: Record<string, unknown>, i: number): string {
  // 没有 rowKey 时退化成"页内下标"会跨页串号，这里退回内容签名
  if (!props.rowKey) {
    return JSON.stringify(r).slice(0, 64) + ":" + i;
  }
  return String(r[props.rowKey]);
}
function isChecked(r: Record<string, unknown>, i: number) {
  return selectedSet.value.has(keyOf(r, i));
}
function toggleRow(r: Record<string, unknown>, i: number) {
  const k = keyOf(r, i);
  const next = new Set(selectedSet.value);
  if (next.has(k)) next.delete(k);
  else next.add(k);
  commit(next);
}
const pageKeys = computed(() => paged.value.map((r, i) => keyOf(r, i)));
const allChecked = computed(
  () => pageKeys.value.length > 0 && pageKeys.value.every((k) => selectedSet.value.has(k)),
);
function togglePage() {
  const next = new Set(selectedSet.value);
  if (allChecked.value) pageKeys.value.forEach((k) => next.delete(k));
  else pageKeys.value.forEach((k) => next.add(k));
  commit(next);
}
function clearSelection() {
  commit(new Set());
}
defineExpose({ clearSelection });

const sortKey = ref("");
const sortDir = ref<1 | -1>(1);
const pageRef = ref(1);
// 受控/非受控两用：外部给了就用外部的，否则用内部 ref
const sortKeyModel = computed({
  get: () => props.sort ?? sortKey.value,
  set: (v: string) => {
    sortKey.value = v;
    emit("update:sort", v);
  },
});
const sortDirModel = computed({
  get: () => (props.dir === "desc" ? -1 : props.dir === "asc" ? 1 : sortDir.value),
  set: (v: 1 | -1) => {
    sortDir.value = v;
    emit("update:dir", v === -1 ? "desc" : "asc");
  },
});
const pageModel = computed({
  get: () => props.page ?? pageRef.value,
  set: (v: number) => {
    pageRef.value = v;
    emit("update:page", v);
  },
});
// 只在"行数变化/筛选变化"时回到第 1 页。
// 监听 props.rows 的数组引用会导致每次 30s 轮询都把用户从第 3 页弹回第 1 页。
watch(
  () => [props.rows.length, props.pageSize],
  () => {
    if (pageModel.value > 1) pageModel.value = 1;
  },
);

const sorted = computed(() => {
  if (!sortKeyModel.value) return props.rows;
  const k = sortKeyModel.value;
  const dir = sortDirModel.value;
  return [...props.rows].sort((a, b) => {
    const x = a[k];
    const y = b[k];
    if (typeof x === "number" && typeof y === "number") return (x - y) * dir;
    const xs = String(x ?? "");
    const ys = String(y ?? "");
    return xs.localeCompare(ys, "zh-CN", { numeric: true }) * dir;
  });
});

const totalPages = computed(() =>
  props.pageSize > 0 ? Math.max(1, Math.ceil(sorted.value.length / props.pageSize)) : 1,
);
// 页码窗口：当前页附近 5 个（原来固定只渲染 1..7，35 页要连点 28 次{{ t("table.next") }}）
const pageWindow = computed(() => {
  const total = totalPages.value;
  const span = 5;
  let start = Math.max(1, pageModel.value - Math.floor(span / 2));
  const end = Math.min(total, start + span - 1);
  start = Math.max(1, end - span + 1);
  const out: number[] = [];
  for (let p = start; p <= end; p += 1) out.push(p);
  return out;
});
const paged = computed(() =>
  props.pageSize > 0
    ? sorted.value.slice((pageModel.value - 1) * props.pageSize, pageModel.value * props.pageSize)
    : sorted.value,
);

function toggleSort(c: Column) {
  if (c.sortable === false) return;
  if (sortKeyModel.value === c.key) sortDirModel.value = (sortDirModel.value * -1) as 1 | -1;
  else {
    sortKeyModel.value = c.key;
    sortDirModel.value = 1;
  }
}
</script>

<template>
  <div class="overflow-x-auto rounded-xl border border-line-2">
    <table class="w-full text-xs">
      <thead class="bg-surface-2 text-3xs tracking-wider text-fg-2 uppercase">
        <tr>
          <th v-if="props.selectable" class="w-9 px-2 py-2">
            <input
              type="checkbox"
              :checked="allChecked"
              class="cursor-pointer accent-[var(--color-primary)]"
              @change="togglePage"
            />
          </th>
          <th
            v-for="c in props.columns"
            :key="c.key"
            class="px-3 py-2 font-medium select-none"
            :class="[
              c.align === 'right' ? 'text-right' : 'text-left',
              c.sortable === false ? '' : 'cursor-pointer hover:text-fg',
            ]"
            :style="c.width ? { width: c.width } : undefined"
            @click="toggleSort(c)"
          >
            {{ c.label }}
            <span v-if="sortKey === c.key" class="ml-0.5 text-primary">{{
              sortDir === 1 ? "↑" : "↓"
            }}</span>
          </th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="(r, i) in paged"
          :key="props.rowKey ? String(r[props.rowKey]) : i"
          class="border-t border-line-2/60 hover:bg-hover/40"
          :class="isChecked(r, i) ? 'bg-primary/5' : ''"
        >
          <td v-if="props.selectable" class="w-9 px-2 py-2">
            <input
              type="checkbox"
              :checked="isChecked(r, i)"
              class="cursor-pointer accent-[var(--color-primary)]"
              @change="toggleRow(r, i)"
            />
          </td>
          <td
            v-for="c in props.columns"
            :key="c.key"
            class="px-3 py-2"
            :class="c.align === 'right' ? 'text-right' : ''"
          >
            <slot :name="`cell-${c.key}`" :row="r" :value="r[c.key]">
              {{ r[c.key] === undefined || r[c.key] === null || r[c.key] === "" ? "—" : r[c.key] }}
            </slot>
          </td>
        </tr>
        <tr v-if="props.error">
          <td :colspan="columns.length" class="px-3 py-6 text-center text-xs text-danger">
            {{ t("table.loadFailed") }}{{ props.error }}
          </td>
        </tr>
        <tr v-else-if="!paged.length">
          <td
            :colspan="props.columns.length + (props.selectable ? 1 : 0)"
            class="px-3 py-8 text-center text-fg-2"
          >
            {{ props.empty }}
          </td>
        </tr>
      </tbody>
    </table>
  </div>

  <div
    v-if="props.pageSize > 0 && sorted.length > props.pageSize"
    class="mt-2 flex items-center gap-2 text-2xs text-fg-2"
  >
    <span>{{
      t("table.range", {
        total: sorted.length,
        page: pageModel,
        pages: totalPages,
      })
    }}</span>
    <div class="ml-auto flex items-center gap-1">
      <button
        class="rounded border border-line px-2 py-0.5 hover:bg-hover disabled:opacity-40"
        :disabled="pageModel <= 1"
        @click="pageModel -= 1"
      >
        {{ t("table.prev") }}
      </button>
      <button
        v-for="p in pageWindow"
        :key="p"
        class="rounded border px-2 py-0.5"
        :class="p === pageModel ? 'border-primary text-primary' : 'border-line hover:bg-hover'"
        @click="pageModel = p"
      >
        {{ p }}
      </button>
      <button
        class="rounded border border-line px-2 py-0.5 hover:bg-hover disabled:opacity-40"
        :disabled="pageModel >= totalPages"
        @click="pageModel += 1"
      >
        {{ t("table.next") }}
      </button>
    </div>
  </div>
</template>
