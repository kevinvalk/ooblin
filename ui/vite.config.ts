import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    target: process.env.TAURI_ENV_PLATFORM == "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
