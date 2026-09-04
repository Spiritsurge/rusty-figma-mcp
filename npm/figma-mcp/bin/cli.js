#!/usr/bin/env node
// Launcher for the platform binary.
//
// The binary is not downloaded at install time. Each platform is its own npm
// package listed under optionalDependencies with `os`/`cpu` set, so npm
// installs exactly the one that matches and skips the rest. That means no
// postinstall script, no network at install time, and nothing that breaks
// behind a proxy or under --ignore-scripts.

import { spawn } from "node:child_process";
import { createRequire } from "node:module";

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
