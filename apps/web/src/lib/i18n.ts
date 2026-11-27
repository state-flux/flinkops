// 极简 i18n：UI 文案集中管理，默认跟随浏览器语言，可手动切换并持久化。
//
// 用法：
//   import { t, lang, setLang } from "../lib/i18n";
//   <span>{{ t("nav.jobs") }}</span>
//
// 约定：key 用「区域.名字」，缺失时回退到中文，再回退到 key 本身
// （开发期一眼能看出哪个 key 没翻译）。新增页面时把文案挪进 messages，
// 不要继续在模板里写死中文。
import { ref, watch } from "vue";

export type Lang = "zh" | "en";
export const LANGS: { id: Lang; label: string }[] = [
  { id: "zh", label: "中文" },
  { id: "en", label: "English" },
];

const STORAGE_KEY = "flinkops.lang";

function detect(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === "zh" || saved === "en") return saved;
    return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
  } catch {
    return "zh";
  }
}

export const lang = ref<Lang>(detect());

watch(
  lang,
  (v) => {
    try {
      localStorage.setItem(STORAGE_KEY, v);
      document.documentElement.lang = v === "zh" ? "zh-CN" : "en";
    } catch {
      /* 隐私模式下 localStorage 可能不可用，忽略 */
    }
  },
  { immediate: true },
);

export function setLang(v: Lang) {
  lang.value = v;
}

export function toggleLang() {
  setLang(lang.value === "zh" ? "en" : "zh");
}

import { zh } from "./i18n/zh";
import { en } from "./i18n/en";

const messages: Record<Lang, Record<string, string>> = { zh, en };

/** 取文案；支持 `{name}` 占位符替换。 */
export function t(key: string, vars?: Record<string, string | number>): string {
  const dict = messages[lang.value] ?? zh;
  let out = dict[key] ?? zh[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      out = out.split(`{${k}}`).join(String(v));
    }
  }
  return out;
}
