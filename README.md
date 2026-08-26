# rusty-figma-mcp

Read the Figma file you have open from Cursor, Claude Code, or any MCP client.
One 3 MB binary, no API token, no REST quotas.

> **Status:** eight read tools, working end to end against real documents.
> Write tools are not implemented.

## Why this exists

Figma's REST API meters tool calls — 6/month on Starter, 200/day on Pro. That
makes agent-driven design work unusable on a free account and awkward on a paid
one.

This talks to a **plugin running in your own Figma session** instead, so there
is no token and no quota. It reads the file you already have open.

Three things make it different from other tools in this space:

- **One binary, no runtime.** 3 MB, statically linked. No Node, no Python, no
  .NET to install alongside it.
- **Documents stream through untouched.** The server parses only the envelope
  around a payload, never the payload itself, so memory stays flat whether the
  document is 100 KB or 100 MB.
- **You can see what the agent is doing.** The plugin panel lists every
  operation as it runs, with live progress and timings.

## How it works

Figma's plugin runtime is split, and neither half can do the job alone: the main
thread has `figma.*` but no network, the UI iframe has network but no `figma.*`.
So anything driving Figma from outside has to relay across that gap.

```
  MCP client (Cursor, Claude Code, …)
        │  stdio, JSON-RPC
        ▼
  figma-mcp ──── listens on localhost:518xx
        │  WebSocket, JSON-RPC
        ▼
  plugin UI iframe
        │  postMessage
        ▼
  plugin main thread ──► figma.*
```

Each MCP client spawns its own server, and each takes the next free port in
51820–51839. The plugin scans that range, lists what it finds, and **you pick
which editor drives Figma** — no leader election, no port races, no guessing
which window is connected.

The wire format is specified in [PROTOCOL.md](PROTOCOL.md).

## Setup

### 1. Build

```sh
cargo build --release              # server → target/release/figma-mcp
cd plugin && npm install && npm run build
```

### 2. Point your MCP client at the binary

```json
{
  "mcpServers": {
    "figma": {
      "command": "/absolute/path/to/target/release/figma-mcp"
    }
  }
}
```

The session appears in the plugin's list under your working directory's name.
Override it with `--label "something else"` if you run several.

### 3. Load the plugin in Figma

Figma desktop → **Plugins → Development → Import plugin from manifest…** and
choose `plugin/manifest.json`.

### 4. Connect

Open a file, run the plugin, and pick your session from the list. The badge goes
green and your MCP client can read the document.

## Tools

| Tool | What it reads |
|---|---|
| `get_metadata` | File name, current page, selection count. Cheap — start here. |
| `get_selection` | The nodes you have selected. Use for "this frame". |
| `get_node` | One node and its subtree, by id. |
| `get_document` | The whole node tree. Large; prefer a `depth` limit. |
| `get_pages` | Pages in the file. |
| `get_styles` | Local paint, text, effect and grid styles. |
| `get_variable_defs` | Variable collections and per-mode values — design tokens. |
| `get_screenshot` | A node or page rendered as an image the model can see. |

### What the output looks like

Colours come back as CSS hex, and anything sitting at its Figma default is
dropped, so what you read is what differs:

```json
{
  "id": "62:17", "name": "Frame 1", "type": "FRAME",
  "width": 430, "height": 932,
  "fills": [{ "type": "SOLID", "opacity": 0.5, "color": "#000000" }],
  "layout": { "mode": "HORIZONTAL", "itemSpacing": 10, "paddingTop": 0 },
  "childCount": 1, "truncated": true
}
```

`truncated` with `childCount` marks where a `depth` limit cut the tree, so a
consumer can tell a real leaf from a boundary and ask for more if it needs to.
`get_document` at `depth: 0` returns a 190-byte outline of a file that runs to
megabytes in full.

`get_screenshot` returns an MCP image block, so the model sees the design rather
than a wall of base64.

## Watching it work

The plugin panel is the only place you can see an agent touching your file, so
it shows the operations rather than a count:

```
●  Reading your selection                    40%
○  Checking the file                        180ms
○  Rendering 127:7                           1.4s
○  Reading design tokens                    620ms
```

Running rows carry live progress; finished ones recede and show how long they
took. Failures show in red with the plugin's own message.

## Security

The server binds loopback and serves one plugin connection at a time.
Authorization is your explicit pick in the plugin UI — a gesture no other local
process can make on your behalf. There is no ambient always-on socket accepting
whatever connects, and the plugin re-checks a server's identity before every
reconnect, so a dead session's port cannot be inherited by another process.

`--bind` accepts a non-loopback address for remote setups. That mode requires a
token, printed at startup, and warns loudly. Don't use it unless you mean it.

## Development

```sh
cargo test                    # 42 tests: protocol, correlation, real sockets
cargo clippy --workspace --all-targets
npm install && npm run smoke  # full path, stub host in place of Figma
cd plugin && npm run typecheck
```

`scripts/smoke.mjs` drives the real binary with a fake plugin, so most protocol
work can be verified without Figma's reload loop. Note that it binds a real port
in the discovery range — close the Figma plugin first, or a live one may attach
to the test's server.

## Prior art

The plugin-relays-to-an-external-server design is imposed by Figma's plugin
sandbox and is well established — notably
[grab/cursor-talk-to-figma-mcp](https://github.com/grab/cursor-talk-to-figma-mcp)
(March 2025) and
[gethopp/figma-mcp-bridge](https://github.com/gethopp/figma-mcp-bridge).

This is an independent implementation: the protocol was specified from Figma's
constraints before any code was written, and diverges deliberately where it is
free to — JSON-RPC framing, user-selected session discovery instead of
leader/follower election, and opaque payload forwarding. See
[PROTOCOL.md](PROTOCOL.md) Appendix A. No source from another implementation was
used.

## License

MIT
