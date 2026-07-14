import { defineConfig } from "vite";

// Vite config tuned for Tauri: fixed dev port, no obscuring of Rust errors.
export default defineConfig({
  // Relative asset URLs keep the same build portable in the Tauri webview and
  // under the repository subpath used by GitHub Pages (/prun/demo/).
  base: "./",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "esnext",
    minify: "esbuild",
  },
});
