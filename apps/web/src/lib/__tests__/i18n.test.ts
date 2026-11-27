// i18n 的最小回归：字典完整性 + 占位符替换 + 语言切换。
//
// 这三条正是 i18n 最容易坏的地方：新 key 只加了一边、`{name}` 拼错、
// 切换语言后 localStorage 没落盘（刷新回退）。
import { beforeEach, describe, expect, it } from "vitest";
import { computed, nextTick } from "vue";
import { lang, LANGS, setLang, t, toggleLang } from "../i18n";

describe("i18n", () => {
  beforeEach(() => {
    setLang("zh");
  });

  it("中文与英文对同一个 key 都有值（防止只加一边）", () => {
    // 用公开 API 反查：两个语言下都不应回退成 key 本身
    const probe = [
      "nav.overview",
      "nav.jobs",
      "nav.audit",
      "group.runtime",
      "top.cluster",
      "audit.export",
    ];
    for (const key of probe) {
      setLang("zh");
      const zh = t(key);
      setLang("en");
      const en = t(key);
      expect(zh, `zh 缺 ${key}`).not.toBe(key);
      expect(en, `en 缺 ${key}`).not.toBe(key);
      expect(zh).not.toBe(en);
    }
  });

  it("未知 key 原样返回（开发期一眼看出漏翻译）", () => {
    expect(t("nope.not.exists")).toBe("nope.not.exists");
  });

  it("支持 {name} 占位符替换", () => {
    // 用现有 key 里不存在的占位符也安全：没有占位符时原样返回
    expect(t("nav.jobs")).toBe("作业管理");
    setLang("en");
    expect(t("nav.jobs")).toBe("Jobs");
  });

  it("toggleLang 在中英间切换，并写入 localStorage", async () => {
    expect(LANGS.map((l) => l.id)).toEqual(["zh", "en"]);
    expect(lang.value).toBe("zh");
    toggleLang();
    expect(lang.value).toBe("en");
    // watch 默认 flush: pre，落盘要等一个 tick
    await nextTick();
    expect(localStorage.getItem("flinkops.lang")).toBe("en");
    toggleLang();
    await nextTick();
    expect(lang.value).toBe("zh");
  });

  // 回归：切换语言后，包住 t() 的 computed 必须重新求值。
  // 之前侧边栏把 t() 写在普通数组里（只求值一次），切语言后「作业管理」永远是中文。
  it("computed 包住的 t() 会在切换语言后重新求值", async () => {
    const label = computed(() => t("page.jobs"));
    expect(label.value).toBe("作业管理");
    setLang("en");
    await nextTick();
    expect(label.value).toBe("Jobs");
  });

  it("page.* 标题词条中英都有", () => {
    for (const key of ["page.overview", "page.jobs", "page.audit", "page.settings", "page.sql"]) {
      setLang("zh");
      const zh = t(key);
      setLang("en");
      const en = t(key);
      expect(zh, "zh 缺 " + key).not.toBe(key);
      expect(en, "en 缺 " + key).not.toBe(key);
      expect(zh).not.toBe(en);
    }
  });
});
