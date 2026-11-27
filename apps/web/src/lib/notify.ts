import { ref } from "vue";

export type Toast = { id: number; text: string; tone: "info" | "ok" | "warn" | "danger" };

export const toasts = ref<Toast[]>([]);
let seq = 0;

export function dismiss(id: number) {
  toasts.value = toasts.value.filter((t) => t.id !== id);
}

export function notify(text: string, tone: Toast["tone"] = "info", ttl = 3200) {
  // 同一句话 1.5s 内只保留一条：连点失败按钮时不该刷出一屏一样的红条
  const now = Date.now();
  const dup = recent.get(text);
  if (dup && now - dup < 1500) {
    const exist = toasts.value.find((t) => t.text === text);
    if (exist) return;
  }
  recent.set(text, now);
  const id = ++seq;
  toasts.value.push({ id, text, tone });
  // 最多 5 条，超出丢最早的（关键错误留在屏幕上）
  if (toasts.value.length > 5) toasts.value = toasts.value.slice(-5);
  setTimeout(() => dismiss(id), ttl);
}

const recent = new Map<string, number>();

/** 设计稿里有、后端还没实现的动作：按钮要在，点了要诚实说明。 */
export function todo(label: string) {
  notify(`「${label}」：接口未实现（见 docs/api-spec.md roadmap）`, "warn");
}

/** 下载文本为文件（导出 CSV / YAML 用）。 */
export function download(filename: string, text: string, mime = "text/plain;charset=utf-8") {
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  // 立刻 revoke 会让 Safari 拿到空文件；延迟释放
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
