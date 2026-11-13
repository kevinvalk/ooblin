import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  // Tauri loads the UI from this port (devUrl), it can't follow Vite elsewhere: one of our own, not Vite's 5173 or
  // Tauri's 1420 that every other project starts on.
  server: { port: 16420, strictPort: true },
  build: {
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
