import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  // Tauri loads the UI from this port (devUrl) and cannot follow Vite to another one. So we use a port of our own
  // instead of the 5173 of Vite or the 1420 of Tauri that every other project starts on.
  server: { port: 16420, strictPort: true },
  build: {
    // NOTE: Tauri only sets this variable (to "true") for a debug build
    minify: process.env.TAURI_ENV_DEBUG === undefined,
    sourcemap: process.env.TAURI_ENV_DEBUG !== undefined,
  },
});
