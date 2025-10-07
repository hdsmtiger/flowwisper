import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(() => ({
  plugins: [react()],
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    clearMocks: true,
    restoreMocks: true,
  },
}));
