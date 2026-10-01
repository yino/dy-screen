import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  plugins: [react()],
  clearScreen: false,
  server: {
    // 开发端口可用 VITE_DEV_PORT 覆盖（默认仍是 1420）。
    // strictPort 保持开启：端口被占时直接失败，避免与 tauri.conf.json 的
    // devUrl 静默不一致。
    port: Number(process.env.VITE_DEV_PORT ?? 1420),
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    css: true,
  },
});
