import { copyFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss(), copyGhosttyWasm()],

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
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));

function copyGhosttyWasm() {
  return {
    name: "copy-ghostty-wasm",
    async closeBundle() {
      const source = resolve("node_modules/ghostty-web/ghostty-vt.wasm");
      const target = resolve("dist/ghostty-vt.wasm");
      await mkdir(dirname(target), { recursive: true });
      await copyFile(source, target);
    },
  };
}
