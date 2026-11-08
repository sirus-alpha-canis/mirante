/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the built files itself; in dev it loads this server (fixed port, no LAN).
export default defineConfig(({ mode }) => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    // Tests only: security.test.ts reads the bridge's allowlist from its Rust source. The
    // dev server the app loads keeps Vite's default (this folder only).
    ...(mode === "test" ? { fs: { allow: [".", "../../crates/mirante-bridge/src"] } } : {}),
  },
  // A desktop app loads its bundle from disk: one chunk is fine.
  build: { target: "safari16", outDir: "dist", sourcemap: false, chunkSizeWarningLimit: 2000 },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
    css: false,
  },
}));
