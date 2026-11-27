<script setup lang="ts">
import { confirmDialog } from "../lib/confirm";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute } from "vue-router";
import { EditorView, basicSetup } from "codemirror";
import { yaml as yamlLang } from "@codemirror/lang-yaml";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as hl } from "@lezer/highlight";
import { editor, jobs, type Finding, type SpecDiff } from "../api/client";
import { notify } from "../lib/notify";
import { useWritable } from "../lib/session";
import { t } from "../lib/i18n";

const props = defineProps<{ cluster?: string; ns?: string; name?: string }>();
const route = useRoute();
const cluster = ref(String(props.cluster ?? route.params.cluster ?? ""));
const ns = ref(String(props.ns ?? route.params.ns ?? ""));
const name = ref(String(props.name ?? route.params.name ?? ""));

const host = ref<HTMLDivElement | null>(null);
const { readonly } = useWritable();
const findings = ref<Finding[]>([]);
const diffs = ref<SpecDiff[]>([]);
const status = ref("");
const busy = ref(false);
const initialSpec = ref<unknown>(null);
/** 载入时的原文：用于判断"有没有未保存修改"（点加载/显示明文前会提醒） */
const initialText = ref("");
const specTree = ref<unknown>(null);
const treeOpen = ref<Record<string, boolean>>({});

let view: EditorView | null = null;

// 亮色主题：白底 + 深灰字 + 柔和高亮。
// 不动 basicSetup 自带的 defaultHighlightStyle（它在浅底上是标准配色），
// 只覆盖背景/行号/选中，避免"语法色被深底吃掉"或"对比过强刺眼"。
const softHighlight = HighlightStyle.define([
  { tag: hl.comment, color: "#94a3b8", fontStyle: "italic" },
  { tag: hl.lineComment, color: "#94a3b8", fontStyle: "italic" },
  { tag: hl.propertyName, color: "#1e40af" },
  { tag: hl.string, color: "#0f766e" },
  { tag: hl.number, color: "#b45309" },
  { tag: hl.bool, color: "#7c3aed" },
  { tag: hl.null, color: "#7c3aed" },
  { tag: hl.keyword, color: "#7c3aed" },
  { tag: hl.tagName, color: "#0f172a" },
  { tag: hl.punctuation, color: "#94a3b8" },
  { tag: hl.invalid, color: "#b91c1c" },
]);

const lightTheme = EditorView.theme(
  {
    "&": { backgroundColor: "#ffffff", color: "#1e293b", fontSize: "12.5px" },
    ".cm-content": {
      fontFamily: "JetBrains Mono, Consolas, monospace",
      padding: "10px 0",
      caretColor: "#2563eb",
    },
    ".cm-gutters": { backgroundColor: "#f8fafc", color: "#94a3b8", border: "1px solid #edf1f6" },
    ".cm-activeLine": { backgroundColor: "#f1f5f9" },
    ".cm-activeLineGutter": { backgroundColor: "#f1f5f9", color: "#475569" },
    "&.cm-focused": { outline: "none" },
    ".cm-selectionBackground, ::selection": { backgroundColor: "#dbeafe !important" },
    ".cm-cursor": { borderLeftColor: "#2563eb" },
  },
  { dark: false },
);

watch(
  () => [route.params.cluster, route.params.ns, route.params.name],
  () => {
    const c = String(route.params.cluster ?? "");
    const n = String(route.params.ns ?? "");
    const nm = String(route.params.name ?? "");
    if (!c || !n || !nm) return;
    cluster.value = c;
    ns.value = n;
    name.value = nm;
    void load(true);
  },
);

// 有未保存修改时关页面/刷新给原生提示（函数声明会提升，可在 text() 定义前使用）
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (initialText.value && text() !== initialText.value) {
    e.preventDefault();
    e.returnValue = "";
  }
}

onMounted(() => {
  window.addEventListener("beforeunload", onBeforeUnload);
  view = new EditorView({
    doc: `${t("ed.tplComment")}\n`,
    extensions: [
      basicSetup,
      yamlLang(),
      lightTheme,
      syntaxHighlighting(softHighlight),
      EditorView.lineWrapping,
    ],
    parent: host.value!,
  });
  // 路由参数齐全时自动加载：从作业行点「编辑配置」进来还要手点一次「加载」
  // 是纯粹的浪费，而且空文档很容易让人以为"这个作业没有配置"。
  if (cluster.value && ns.value && name.value) void load();
});

onBeforeUnmount(() => {
  window.removeEventListener("beforeunload", onBeforeUnload);
  view?.destroy();
});

const text = () => view?.state.doc.toString() ?? "";
const setText = (t: string) => {
  if (!view) return;
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: t } });
};

async function load(force = false) {
  // 有未保存改动时先确认：点「加载/显示明文」会直接覆盖文本框
  if (!force && initialText.value && text() !== initialText.value) {
    if (
      !(await confirmDialog({
        message: t("ed.unsavedConfirm"),
        danger: true,
      }))
    )
      return;
  }
  busy.value = true;
  status.value = "";
  diffs.value = [];
  try {
    const doc = await editor.jobYaml(cluster.value, ns.value, name.value, !redacted.value);
    setText(doc);
    initialText.value = doc;
    // 记下"集群当前 spec"，供 diff 使用
    const v = await editor.validate(doc);
    initialSpec.value = v.spec;
    specTree.value = v.spec;
    findings.value = v.findings;
    status.value = redacted.value ? t("ed.loadedRedacted") : t("ed.loadedPlain");
  } catch (e) {
    status.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

// 默认打码；编辑含凭据的 CR 需要显式解锁（保存时后端也会拒绝打码占位符）
const redacted = ref(true);
async function reveal() {
  if (
    !(await confirmDialog({
      message: t("ed.revealConfirm"),
    }))
  )
    return;
  redacted.value = false;
  await load();
}

async function check() {
  busy.value = true;
  try {
    const v = await editor.validate(text());
    findings.value = v.findings;
    specTree.value = v.spec;
    status.value = v.hasErrors ? t("ed.invalid") : t("ed.valid");
  } catch (e) {
    status.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function compare() {
  busy.value = true;
  try {
    const v = await editor.validate(text());
    const d = await editor.diff(initialSpec.value ?? {}, v.spec);
    diffs.value = d.diffs;
    status.value = d.count ? `与集群有 ${d.count} 处差异` : t("ed.noDrift");
  } catch (e) {
    status.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

const sevClass = (s: string) =>
  s === "error" ? "text-danger" : s === "warn" ? "text-warn" : "text-fg-2";

// 格式化：只做 YAML 解析 + 重新序列化（/api/format），不经过基线渲染。
// 早期实现走 render()，会丢 apiVersion/kind/metadata 并把基线默认键倒灌进来。
async function format() {
  busy.value = true;
  try {
    const r = await editor.format(text());
    setText(r.yaml);
    status.value = t("ed.formatted");
  } catch (e) {
    status.value = t("ed.formatFailed", { err: (e as Error).message });
  } finally {
    busy.value = false;
  }
}

// 保存到集群（服务端 PUT + resourceVersion CAS）
async function save() {
  if (!cluster.value || !ns.value || !name.value) {
    notify(t("ed.needIdentity"), "warn");
    return;
  }
  // 打码态（含 **** 占位符）保存必然被后端拒绝，那是防止把占位符写回集群的守卫。
  // 与其让用户对着 400 猜，不如直接切明文重载一遍。
  if (redacted.value) {
    if (
      !(await confirmDialog({
        message: t("ed.redactedConfirm"),
      }))
    )
      return;
    redacted.value = false;
    await load();
    notify(t("ed.switchedPlain"), "warn");
    return;
  }
  if (
    !(await confirmDialog({
      message: t("ed.saveConfirm", {
        cluster: cluster.value,
        ns: ns.value,
        name: name.value,
      }),
    }))
  )
    return;
  busy.value = true;
  try {
    const r = await jobs.update(cluster.value, ns.value, name.value, text());
    initialText.value = text();
    status.value = t("ed.saved", { n: r.changed ?? 0 });
    notify(t("ed.saved", { n: r.changed ?? 0 }), "ok");
    const v = await editor.validate(text());
    initialSpec.value = v.spec;
    findings.value = v.findings;
  } catch (e) {
    status.value = t("ed.saveFailed", { err: (e as Error).message });
    notify(t("ed.saveFailed", { err: (e as Error).message }), "danger");
  } finally {
    busy.value = false;
  }
}

function copyKubectl() {
  void navigator.clipboard?.writeText(`cat <<'EOF' | kubectl apply -f -\n${text()}\nEOF`);
  notify(t("ed.copiedKubectl"), "ok");
}

// 字段树：从 spec 递归展开（最多 3 层，避免长文本撑爆侧栏）
type Node = { key: string; value?: string; children?: Node[] };
const tree = computed<Node[]>(() => {
  const build = (obj: unknown, depth: number): Node[] => {
    if (!obj || typeof obj !== "object" || depth > 3) return [];
    return Object.entries(obj as Record<string, unknown>).map(([k, v]) => {
      if (v && typeof v === "object" && !Array.isArray(v)) {
        return { key: k, children: build(v, depth + 1) };
      }
      if (Array.isArray(v)) return { key: k, value: `[${v.length}]` };
      return { key: k, value: String(v) };
    });
  };
  return build(specTree.value ?? {}, 0);
});

// 配置字典：高频键 + 说明。点一下复制键名（不直接改文档，避免新手误改）
const DICT: { k: string; desc: string }[] = [
  { k: "execution.checkpointing.interval", desc: t("ed.hint.ckptInterval") },
  { k: "execution.checkpointing.timeout", desc: t("ed.hint.ckptTimeout") },
  { k: "execution.checkpointing.mode", desc: "EXACTLY_ONCE / AT_LEAST_ONCE" },
  {
    k: "execution.checkpointing.tolerable-failed-checkpoints",
    desc: t("ed.hint.ckptTolerable"),
  },
  { k: "execution.checkpointing.min-pause", desc: t("ed.hint.ckptMinPause") },
  { k: "state.backend", desc: t("ed.hint.stateBackend") },
  { k: "state.backend.incremental", desc: t("ed.hint.incremental") },
  { k: "state.checkpoints.dir", desc: t("ed.hint.ckptDir") },
  { k: "state.savepoints.dir", desc: t("ed.hint.spDir") },
  { k: "state.checkpoints.num-retained", desc: t("ed.hint.retained") },
  { k: "restart-strategy", desc: "exponential-delay / fixed-delay / failure-rate" },
  { k: "taskmanager.numberOfTaskSlots", desc: t("ed.hint.slots") },
  { k: "kubernetes.rest-service.exposed.type", desc: "ClusterIP / NodePort" },
  { k: "kubernetes.operator.periodic.savepoint.interval", desc: t("ed.hint.periodic") },
  { k: "metrics.reporter.prom.factory.class", desc: t("ed.hint.prom") },
];
const dictKw = ref("");
const dictView = computed(() =>
  DICT.filter((d) => !dictKw.value || d.k.toLowerCase().includes(dictKw.value.toLowerCase())),
);
const copyKey = (k: string) => {
  void navigator.clipboard?.writeText(k);
  notify(t("ed.copiedKey", { key: k }), "ok");
};
</script>

<template>
  <h1 class="mb-1 text-[20px] font-semibold">{{ t("ed.title") }}</h1>
  <p class="mb-3 text-xs text-fg-2">{{ t("ed.sub") }}</p>

  <div class="mb-3 flex flex-wrap items-center gap-2 text-xs">
    <input
      v-model="cluster"
      placeholder="cluster"
      class="w-28 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
    />
    <input
      v-model="ns"
      placeholder="namespace"
      class="w-36 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
    />
    <input
      v-model="name"
      placeholder="job name"
      class="w-44 rounded-lg border border-line bg-surface-2 px-2 py-1 font-mono"
    />
    <button
      :disabled="busy"
      class="rounded-lg bg-surface-3 px-3 py-1 hover:bg-hover disabled:opacity-50"
      @click="load(false)"
    >
      {{ t("ed.load") }}
    </button>
    <button
      :disabled="busy || !redacted"
      class="rounded-lg bg-warn/20 px-3 py-1 text-warn hover:bg-warn/30 disabled:opacity-40"
      @click="reveal"
    >
      {{ t("ed.reveal") }}
    </button>
    <button
      :disabled="busy"
      class="rounded-lg bg-primary px-3 py-1 text-white hover:opacity-90 disabled:opacity-50"
      @click="check"
    >
      {{ t("ed.validate") }}
    </button>
    <button
      :disabled="busy"
      class="rounded-lg bg-surface-3 px-3 py-1 hover:bg-hover disabled:opacity-50"
      @click="compare"
    >
      {{ t("ed.diff") }}
    </button>
    <button
      :disabled="busy"
      class="rounded-lg bg-surface-3 px-3 py-1 hover:bg-hover disabled:opacity-50"
      @click="format"
    >
      {{ t("ed.format") }}
    </button>
    <button
      :disabled="busy || readonly"
      class="rounded-lg bg-ok px-3 py-1 text-white hover:opacity-90 disabled:opacity-50"
      @click="save"
    >
      {{ t("ed.save") }}
    </button>
    <button class="rounded-lg bg-surface-3 px-3 py-1 hover:bg-hover" @click="copyKubectl">
      {{ t("ed.copyKubectl") }}
    </button>
    <span v-if="status" class="text-fg-2">{{ status }}</span>
  </div>

  <div class="grid gap-3" style="grid-template-columns: minmax(0, 1fr) 320px">
    <div
      ref="host"
      class="overflow-hidden rounded-xl border border-line-2 bg-white"
      style="min-height: 520px"
    ></div>

    <div class="flex flex-col gap-3">
      <!-- 校验结果 -->
      <div class="rounded-xl border border-line-2 bg-surface p-3">
        <div class="mb-2 text-2xs text-fg-2">{{ t("ed.findings") }}{{ findings.length }}）</div>
        <div v-for="(f, i) in findings" :key="i" class="border-t border-line-2/60 py-1.5 text-2xs">
          <div class="flex items-center gap-2">
            <span :class="sevClass(f.severity)" class="uppercase">{{ f.severity }}</span>
            <span class="font-mono text-3xs text-fg-2">{{ f.code }}</span>
          </div>
          <div class="mt-0.5 text-fg-1">{{ f.message }}</div>
          <div class="font-mono text-3xs text-fg-2">{{ f.path }}</div>
        </div>
        <div v-if="!findings.length" class="text-2xs text-fg-2">{{ t("ed.noFindings") }}</div>
      </div>

      <!-- 字段树 -->
      <div class="rounded-xl border border-line-2 bg-surface p-3">
        <div class="mb-2 flex items-center gap-2 text-2xs text-fg-2">
          <span>{{ t("ed.fieldTree") }}</span>
          <button class="rounded border border-line px-1.5 py-0.5 text-3xs" @click="treeOpen = {}">
            {{ t("ed.collapse") }}
          </button>
        </div>
        <div class="max-h-[260px] overflow-auto text-2xs">
          <template v-for="(n, i) in tree" :key="n.key + i">
            <div
              v-if="!n.children"
              class="flex gap-2 border-b border-line-2/40 py-0.5 last:border-0"
            >
              <span class="w-[56%] truncate font-mono text-fg-1" :title="n.key">{{ n.key }}</span>
              <span class="truncate font-mono text-fg-2" :title="n.value">{{ n.value }}</span>
            </div>
            <div v-else>
              <button
                class="flex w-full items-center gap-1 py-0.5 text-left font-mono text-fg-1"
                @click="treeOpen[n.key] = !treeOpen[n.key]"
              >
                <span class="text-fg-2">{{ treeOpen[n.key] === false ? "▸" : "▾" }}</span>{{
                  n.key
                }}
                <span class="text-3xs text-fg-2">({{ n.children.length }})</span>
              </button>
              <div v-if="treeOpen[n.key] !== false" class="ml-3 border-l border-line-2/60 pl-2">
                <div v-for="c in n.children" :key="c.key" class="flex gap-2 py-0.5">
                  <span class="w-[52%] truncate font-mono text-fg-2" :title="c.key">{{
                    c.key
                  }}</span>
                  <span class="truncate font-mono text-3xs text-fg-2" :title="c.value">{{
                    c.value
                  }}</span>
                </div>
              </div>
            </div>
          </template>
          <div v-if="!tree.length" class="text-2xs text-fg-2">{{ t("ed.needSpec") }}</div>
        </div>
      </div>

      <!-- 配置字典 -->
      <div class="rounded-xl border border-line-2 bg-surface p-3">
        <div class="mb-2 flex items-center gap-2 text-2xs text-fg-2">
          <span>{{ t("ed.cfgDict") }}</span>
          <input
            v-model="dictKw"
            :placeholder="t('ed.filterKeys')"
            class="ml-auto w-28 rounded border border-line bg-surface-2 px-1.5 py-0.5 text-3xs"
          />
        </div>
        <div class="max-h-[240px] overflow-auto">
          <button
            v-for="d in dictView"
            :key="d.k"
            class="block w-full border-b border-line-2/40 py-1 text-left last:border-0 hover:bg-hover"
            @click="copyKey(d.k)"
          >
            <div class="font-mono text-3xs text-fg-1">{{ d.k }}</div>
            <div class="text-3xs text-fg-2">{{ d.desc }}</div>
          </button>
        </div>
        <p class="mt-1.5 text-3xs text-fg-2">{{ t("ed.cfgNote") }}</p>
      </div>

      <!-- 差异 -->
      <div class="rounded-xl border border-line-2 bg-surface p-3">
        <div class="mb-2 text-2xs text-fg-2">{{ t("ed.diffTitle") }}{{ diffs.length }}）</div>
        <div v-for="(d, i) in diffs" :key="i" class="border-t border-line-2/60 py-1.5 text-2xs">
          <div class="font-mono text-fg-1">{{ d.path }}</div>
          <div class="text-danger">- {{ d.before ?? t("ed.none") }}</div>
          <div class="text-ok">+ {{ d.after ?? t("ed.none") }}</div>
        </div>
        <div v-if="!diffs.length" class="text-2xs text-fg-2">{{ t("ed.diffHint") }}</div>
      </div>
    </div>
  </div>
</template>
