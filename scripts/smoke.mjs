// End-to-end smoke test: drives the real binary with a stub host in place of
// the Figma plugin, so the whole MCP -> link -> host -> MCP path is exercised
// without opening Figma.
//
//   node scripts/smoke.mjs [path-to-binary]

import { spawn } from "node:child_process";
import { WebSocket } from "ws";

const BIN = process.argv[2] ?? "./target/debug/figma-mcp.exe";
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;

function check(name, cond, detail = "") {
  console.log(`${cond ? "  ok  " : "FAIL  "}${name}${detail ? ` — ${detail}` : ""}`);
  if (!cond) failures++;
}

const server = spawn(BIN, ["--label", "smoke-test"], { stdio: ["pipe", "pipe", "pipe"] });
let stderr = "";
server.stderr.on("data", (d) => (stderr += d.toString()));

const pending = new Map();
let buffer = "";
server.stdout.on("data", (chunk) => {
  buffer += chunk.toString();
  let nl;
  while ((nl = buffer.indexOf("\n")) !== -1) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    const msg = JSON.parse(line);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  }
});

let nextId = 1;
function rpc(method, params) {
  const id = nextId++;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    server.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
}

try {
  await wait(600);

  // The port the plugin would find by scanning the range.
  const port = Number(stderr.match(/listening port=(\d+)/)?.[1]);
  check("server bound a port in range", port >= 51820 && port <= 51839, `:${port}`);

  const hello = await fetch(`http://127.0.0.1:${port}/hello`).then((r) => r.json());
  check("/hello identifies the session", hello.host === "figma" && hello.label === "smoke-test");

  const init = await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "smoke", version: "0" },
  });
  check("MCP initialize", init.result?.serverInfo?.name === "figma-mcp");
  server.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");

  // A tool call with nothing connected must explain itself, not hang.
  const orphan = await rpc("tools/call", { name: "get_metadata", arguments: {} });
  const orphanText = orphan.result?.content?.[0]?.text ?? "";
  check("disconnected call explains the fix", orphan.result?.isError === true && orphanText.includes("plugin"), orphanText.slice(0, 48));

  // Stand up the stub host, exactly as the plugin UI does.
  const host = new WebSocket(`ws://127.0.0.1:${port}/link?v=1`);
  await new Promise((res, rej) => { host.onopen = res; host.onerror = rej; });
  check("host connected over the link", host.readyState === WebSocket.OPEN);

  host.on("message", async (raw) => {
    const req = JSON.parse(raw.toString());
    if (req.method === "figma/getDocument") {
      // Report progress, then answer after the original deadline would have passed.
      for (const pct of [30, 60, 90]) {
        await wait(120);
        host.send(JSON.stringify({ jsonrpc: "2.0", method: "$/progress", params: { id: req.id, pct } }));
      }
      host.send(JSON.stringify({ jsonrpc: "2.0", id: req.id, result: { name: "Doc", pages: [] } }));
    } else if (req.method === "figma/getNode") {
      host.send(JSON.stringify({ jsonrpc: "2.0", id: req.id, error: { code: -32004, message: "no node with id 9:99" } }));
    } else {
      host.send(JSON.stringify({ jsonrpc: "2.0", id: req.id, result: { fileName: "Smoke", pageName: "Page 1", selectionCount: 0 } }));
    }
  });
  await wait(150);

  const meta = await rpc("tools/call", { name: "get_metadata", arguments: {} });
  const metaText = meta.result?.content?.[0]?.text ?? "";
  check("tool call round trip", metaText.includes("Smoke"), metaText.slice(0, 48));
  check("payload arrives verbatim", metaText === '{"fileName":"Smoke","pageName":"Page 1","selectionCount":0}');

  const doc = await rpc("tools/call", { name: "get_document", arguments: { depth: 2 } });
  check("progress kept a slow call alive", (doc.result?.content?.[0]?.text ?? "").includes("Doc"));

  const missing = await rpc("tools/call", { name: "get_node", arguments: { node_id: "9:99" } });
  check("host error reaches the caller", (missing.result?.content?.[0]?.text ?? "").includes("9:99"));

  host.close();
  await wait(200);
  const afterClose = await rpc("tools/call", { name: "get_metadata", arguments: {} });
  check("disconnect is detected", (afterClose.result?.content?.[0]?.text ?? "").includes("plugin"));
} finally {
  server.kill();
}

console.log(failures === 0 ? "\nall checks passed" : `\n${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
