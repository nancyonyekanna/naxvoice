import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri reads `devUrl` from tauri.conf.json and expects this exact port, so it
// must not silently fall back to another one when 1420 is busy — a dev build
// pointing at a dead port looks like a blank window rather than an error.
export default defineConfig({
  plugins: [react()],
  server: { port: 1420, strictPort: true },
  // `public/` is copied verbatim. overlay.html lives there on purpose: it is a
  // standalone page that talks to the `window.__TAURI__` global rather than
  // importing anything, so bundling it would only give it a chance to break.
  build: { outDir: "dist", emptyOutDir: true },
  clearScreen: false,
});
