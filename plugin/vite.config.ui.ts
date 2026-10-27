import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// The release the plugin ships with, from the same Cargo.toml the server's
// version comes from, so the two cannot be stamped differently.
const version = readFileSync("../Cargo.toml", "utf8").match(/^version = "([^"]+)"/m)?.[1];
if (!version) throw new Error("no version found in ../Cargo.toml");

// The UI iframe. Figma loads it from a single inlined HTML file, so every asset
// has to end up in the document itself.
export default defineConfig({
  root: "src/ui",
  define: { __PLUGIN_VERSION__: JSON.stringify(version) },
  plugins: [react(), viteSingleFile()],
  build: {
    outDir: "../../dist",
    emptyOutDir: true,
    target: "es2017",
    cssCodeSplit: false,
    assetsInlineLimit: 100_000_000,
  },
});
