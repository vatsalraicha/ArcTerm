import { defineConfig } from "vite";

// Tauri expects a fixed dev server port; HMR over that same port.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // Spec-mandated layout: src/index.html, src/main.ts, etc. Tell Vite where
  // to look so the standard template paths resolve.
  root: "src",
  publicDir: "../public",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: {
      // Don't reload Vite when Rust source changes — Tauri handles that.
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Output relative to root: ../dist resolves to apps/desktop/dist, which
    // matches frontendDist in tauri.conf.json.
    outDir: "../dist",
    emptyOutDir: true,
    target: "es2022",
    minify: "esbuild",
    sourcemap: true,
  },
});
