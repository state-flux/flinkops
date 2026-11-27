import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";

// 开发时 /api 与 /proxy 都代理到本地 axum（:8080），
// 生产构建产物直接由 axum 的 ServeDir 托管，无需再跑 Node。
export default defineConfig({
  // 把构建时间打进产物：页面控制台会打印 [flinkops] web build=<时间>。
  // 排查"为什么文案还是旧的"时，先看这一行——多半是浏览器缓存了旧 bundle。
  define: {
    __BUILD_TIME__: JSON.stringify(new Date().toISOString()),
  },
  plugins: [vue(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: true },
      "/proxy": { target: "http://127.0.0.1:8080", changeOrigin: true },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: false,
    rollupOptions: {
      output: {
        // Vite 8 要求函数式 manualChunks
        manualChunks(id: string) {
          if (id.includes("@codemirror") || id.includes("/codemirror/")) return "codemirror";
          if (id.includes("/uplot/")) return "charts";
          return undefined;
        },
      },
    },
  },
});