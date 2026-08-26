import { defineConfig } from "vite";

// The plugin main thread. Figma evaluates this in a sandbox with no module
// loader, so it must come out as one self-contained IIFE.
export default defineConfig({
  build: {
    outDir: "dist",
    emptyOutDir: false,
    target: "es2017",
    lib: { entry: "src/main.ts", formats: ["iife"], name: "plugin", fileName: () => "code.js" },
    rollupOptions: { output: { extend: true } },
  },
});
