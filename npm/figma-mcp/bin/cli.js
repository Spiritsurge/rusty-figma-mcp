#!/usr/bin/env node
// Launcher for the platform binary.
//
// The binary is not downloaded at install time. Each platform is its own npm
// package listed under optionalDependencies with `os`/`cpu` set, so npm
// installs exactly the one that matches and skips the rest. That means no
// postinstall script, no network at install time, and nothing that breaks
// behind a proxy or under --ignore-scripts.

import { spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);

const PACKAGES = {
  "darwin arm64": "@spiritsurge/figma-mcp-darwin-arm64",
  "darwin x64": "@spiritsurge/figma-mcp-darwin-x64",
  "linux arm64": "@spiritsurge/figma-mcp-linux-arm64",
  "linux x64": "@spiritsurge/figma-mcp-linux-x64",
  "win32 arm64": "@spiritsurge/figma-mcp-win32-arm64",
  "win32 x64": "@spiritsurge/figma-mcp-win32-x64",
};

// Diagnostics go to stderr without exception: stdout carries the MCP
// transport, and a single stray line on it corrupts the stream.
function fail(message) {
  process.stderr.write(`figma-mcp: ${message}\n`);
  process.exit(1);
}

// `install-plugin` copies the Figma plugin that shipped with this release to a
// fixed folder, so a new release overwrites the files there instead of landing
// in a new folder the user has to import again. Figma re-reads the plugin's
// files from the imported folder each time it runs.
//
//   figma-mcp install-plugin [--dir <path>]
//
// Files are overwritten one by one and nothing is ever deleted: a Figma session
// that is running the plugin keeps working, and any other plugin folder the
// user has imported is left alone.
function installPlugin(args) {
  const flag = args.indexOf("--dir");
  const target = resolve(flag === -1 ? join(homedir(), ".figma-mcp", "plugin") : (args[flag + 1] ?? ""));
  if (flag !== -1 && !args[flag + 1]) fail("--dir needs a path");

  const source = join(dirname(fileURLToPath(import.meta.url)), "..", "plugin");
  const files = ["manifest.json", "dist/code.js", "dist/index.html"];
  const missing = files.filter((file) => !existsSync(join(source, file)));
  if (missing.length > 0) {
    fail(`this package carries no plugin files (${missing.join(", ")}). Install a published release: npx @spiritsurge/figma-mcp install-plugin`);
  }

  // Data files first, the manifest last, so a reader that sees the new
  // manifest never finds the old code behind it.
  mkdirSync(join(target, "dist"), { recursive: true });
  for (const file of [...files.slice(1), files[0]]) copyFileSync(join(source, file), join(target, file));

  process.stdout.write(
    `Plugin written to ${target}\n\n` +
      `First time only: in Figma, Plugins > Development > Import plugin from manifest,\n` +
      `then choose ${join(target, "manifest.json")}\n` +
      `After that, re-run this command on each release; no re-import is needed.\n`,
  );
  process.exit(0);
}

if (process.argv[2] === "install-plugin") installPlugin(process.argv.slice(3));

const key = `${process.platform} ${process.arch}`;
const pkg = PACKAGES[key];

if (!pkg) {
  fail(
    `no prebuilt binary for ${key}.\n` +
      `  Supported: ${Object.keys(PACKAGES).join(", ")}\n` +
      `  Build from source instead: cargo install --git https://github.com/Spiritsurge/rusty-figma-mcp`,
  );
}

const exe = process.platform === "win32" ? "figma-mcp.exe" : "figma-mcp";

let binary;
try {
  binary = require.resolve(`${pkg}/bin/${exe}`);
} catch {
  fail(
    `the binary package ${pkg} is not installed.\n` +
      `  This usually means the install skipped optional dependencies.\n` +
      `  Try: npm install ${pkg}`,
  );
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });

// Forward termination so an MCP client stopping this process stops the server
// too, rather than leaving it holding a port.
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

child.on("error", (error) => fail(`could not start ${binary}: ${error.message}`));
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 0);
});
