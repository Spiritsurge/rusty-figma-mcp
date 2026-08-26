# rusty-figma-mcp

Read the Figma file you have open from Cursor, Claude Code, or any MCP client.
One 3 MB binary, no API token, no REST quotas.

> **Status:** eight read tools and four write tools, working end to end
> against real documents.

## Why this exists

Figma's REST API meters tool calls — 6/month on Starter, 200/day on Pro. That
makes agent-driven design work unusable on a free account and awkward on a paid
one.

This talks to a **plugin running in your own Figma session** instead, so there
is no token and no quota. It reads — and edits — the file you already have open.

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

### 1. Point your MCP client at the server

```json
{
  "mcpServers": {
    "figma": { "command": "npx", "args": ["-y", "rusty-figma-mcp"] }
  }
}
```

Node is used only to fetch and launch the binary, never to run it — the server
is a static executable with no runtime dependency. Prebuilt binaries ship as
platform packages under `optionalDependencies`, so npm installs exactly the one
your machine needs and nothing is downloaded at install time.

Prefer no Node at all? Take the binary for your platform from
[Releases](https://github.com/Spiritsurge/rusty-figma-mcp/releases) and give its
path as `command`.

The session appears in the plugin's list under your working directory's name.
Override it with `--label "something else"` if you run several.

### 2. Install the Figma plugin

The plugin is the other half of the bridge. Download `figma-plugin.zip` from
[Releases](https://github.com/Spiritsurge/rusty-figma-mcp/releases), unzip it,
then in Figma desktop go to **Plugins → Development → Import plugin from
manifest…** and choose `plugin/manifest.json`.

### 3. Connect

Open a file, run the plugin, and pick your session from the list. The badge goes
green and your MCP client can read the document.

### Building from source

```sh
cargo build --release              # server → target/release/figma-mcp
cd plugin && npm install && npm run build
```

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

### Writing

Every one of these changes the document. Figma's own undo works normally, and
the plugin panel names each operation as it happens.

| Tool | What it does |
|---|---|
| `clone_node` | Duplicate a node with children, styles and effects intact. |
| `delete_nodes` | Remove nodes by id. Missing ids are reported, not fatal. |
| `set_text` | Replace a text node's contents, keeping its formatting. |
| `create_image` | Place a PNG, JPEG or GIF from disk as a new layer. |

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

## Making a variant

The useful pattern is **clone, then subtract** — not read, then rebuild:

```
clone_node("90:30", x=2472, y=255)   → an exact copy, new ids
delete_nodes([...])                   → strip what the variant does not need
set_text("2007:63", "trial bonus")    → retext what is left
```

Rebuilding from a read means re-deriving every gradient transform, drop shadow
and image hash, and silently losing anything the serializer did not capture. A
clone is exact by construction, so deleting a few layers afterwards is the whole
job.

`set_text` returns each node's `autoResize`, which is what to watch when
localising: a node set to `NONE` keeps its width and will overflow on a longer
translation, while `WIDTH_AND_HEIGHT` grows to fit.

## Watching it work

The plugin panel is the only place you can see an agent touching your file, so
it shows the operations rather than a count:

```
●  Reading your selection                    40%
○  Changing text                            210ms
○  Duplicating a layer                       1.1s
○  Rendering 127:7                           1.4s
```

Running rows carry live progress; finished ones recede and show how long they
took. Failures show in red with the plugin's own message.

This matters more for writes than reads. An agent changing a file you cannot
watch is an uncomfortable place to be, so every write appears here by name.

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
cargo test                    # 44 tests: protocol, correlation, real sockets
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
leader/follower election, and opaque payload forwarding. No source from another
implementation was used.

That is checkable rather than merely claimed. `scripts/similarity.mjs` compares
this source against any other project; run against all four implementations
above, the longest run of consecutive identical lines is **seven**, and those
seven are a rectangle'''s geometry in Figma'''s own property order. See
[PROTOCOL.md](PROTOCOL.md) Appendix A.

## License

MIT
