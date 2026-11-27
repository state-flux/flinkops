import { defineConfig } from "vitest/config";

// 前端单测跑在 node 环境：只测纯逻辑（确认队列 / i18n / 状态判定），
// 需要 DOM 的组件测试再单独开 environment: "jsdom"。
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    setupFiles: ["src/lib/__tests__/setup.ts"],
  },
});
