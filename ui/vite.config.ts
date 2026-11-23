import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 移动端需要固定端口 + 不自动切换
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "0.0.0.0",
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "chrome107",
    minify: "esbuild",
    sourcemap: false,
  },
});
