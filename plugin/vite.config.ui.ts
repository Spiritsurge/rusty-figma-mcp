import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// The UI iframe. Figma loads it from a single inlined HTML file, so every asset
// has to end up in the document itself.
export default defineConfig({
  root: "src/ui",
  plugins: [react(), viteSingleFile()],
  build: {
    outDir: "../../dist",
    emptyOutDir: true,
    target: "es2017",
    cssCodeSplit: false,
    assetsInlineLimit: 100_000_000,
  },
});
