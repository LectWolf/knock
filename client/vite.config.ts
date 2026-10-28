import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// 客户端只跟 Rust 侧的状态机打交道，Vite 这边不需要任何特殊配置。
export default defineConfig({
  plugins: [vue()],
  // Tauri 用固定端口，避免它自己找端口
  server: { port: 5173, strictPort: true },
  build: { target: "chrome105", outDir: "dist" },
});
