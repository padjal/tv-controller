// vitest's defineConfig accepts the `test` block as well as vite's own options.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": {
        target: "http://localhost:8000",
        // SSE must not be buffered by the dev proxy, or events arrive in
        // batches (or not at all) during development.
        changeOrigin: true,
      },
      "/videos": "http://localhost:8000",
    },
  },
  build: {
    // Served by the Rust server's DASHBOARD_DIR fallback.
    outDir: "../server/dashboard/dist",
    emptyOutDir: true,
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
