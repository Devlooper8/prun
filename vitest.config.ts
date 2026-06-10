import { defineConfig } from "vitest/config";

// Unit tests for the pure frontend logic (format/grouping). Node environment —
// these functions carry no DOM dependency by design. Kept separate from
// vite.config.ts so the Tauri build config stays untouched.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
