import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  // 多页面入口：主窗口 index.html + 悬浮窗 overlay.html（各自独立 bundle）
  build: {
    rollupOptions: {
      input: {
        main: "index.html",
        overlay: "overlay.html",
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`，以及独立 POC 工程 `experiments`
      //    （experiments 里的 cargo 构建会大量改动 target/，会触发 Vite 监视器 EBUSY 崩溃）
      ignored: ["**/src-tauri/**", "**/experiments/**"],
    },
  },
}));
