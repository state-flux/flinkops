// ESLint 9 flat config（前端工程化基线）
//
// 目标：先把"能跑起来"的门禁建好 —— 规则取 Vue 官方 essential + TS 推荐集里
// 不误报的部分，历史上大量 `any` 与显式断言暂不阻断（见 no-explicit-any）。
// 首次接入时建议先本地 `npm run lint` 清一遍，再把 CI 的 continue-on-error 去掉。
import js from "@eslint/js";
import ts from "typescript-eslint";
import vue from "eslint-plugin-vue";

export default [
  { ignores: ["dist/**", "node_modules/**", "scripts/**"] },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...vue.configs["flat/essential"],
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: { parser: ts.parser, ecmaVersion: "latest", sourceType: "module" },
    },
  },
  {
    // 浏览器/ES2022 全局：TS 本身会检查未定义变量，no-undef 交给配置兜底即可
    languageOptions: {
      globals: {
        window: "readonly", document: "readonly", navigator: "readonly",
        localStorage: "readonly", sessionStorage: "readonly", location: "readonly",
        fetch: "readonly", Headers: "readonly", Request: "readonly", Response: "readonly",
        URL: "readonly", URLSearchParams: "readonly", Blob: "readonly", File: "readonly",
        FormData: "readonly", AbortController: "readonly", AbortSignal: "readonly",
        setTimeout: "readonly", clearTimeout: "readonly", setInterval: "readonly",
        clearInterval: "readonly", console: "readonly", crypto: "readonly",
        performance: "readonly", HTMLElement: "readonly", Event: "readonly",
        KeyboardEvent: "readonly", MouseEvent: "readonly", CustomEvent: "readonly",
        requestAnimationFrame: "readonly", cancelAnimationFrame: "readonly",
        getComputedStyle: "readonly", Image: "readonly", FileReader: "readonly",
        Worker: "readonly", WebSocket: "readonly", ResizeObserver: "readonly",
        IntersectionObserver: "readonly", MutationObserver: "readonly",
      },
    },
  },
  {
    files: ["**/*.{ts,vue}"],
    rules: {
      // tsc 已经管未定义变量，ESLint 的 no-undef 只会在 TS 场景误报
      "no-undef": "off",
    },
  },
  {
    rules: {
      // 视图组件名多为单词（Jobs.vue / Sql.vue），约定俗成的例外
      "vue/multi-word-component-names": "off",
      // 历史代码里 46 处 any，先降级为提醒，逐步收敛到 types.ts
      "@typescript-eslint/no-explicit-any": "warn",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
      "no-empty": ["error", { allowEmptyCatch: true }],
    },
  },
];
