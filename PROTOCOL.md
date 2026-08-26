# Host Link Protocol (HLP) v0.1

A transport for driving a sandboxed host application (Figma, Unity, Blender, …)
from an external MCP server.

This document is written from the constraints below and is the normative
reference for both halves of the implementation. Wire changes land here first.

---

## 1. Why a protocol is needed at all

Figma's plugin runtime is split in two, and neither half can do the job alone:

| | `figma.*` API | Network |
|---|---|---|
| Plugin main thread (`code.js`) | yes | **no** |
| Plugin UI iframe | **no** | yes |

The two halves communicate only by `postMessage`. So anything that drives Figma
from outside *must* be: external process ⇄ network ⇄ UI iframe ⇄ postMessage ⇄
main thread ⇄ `figma.*`. This shape is imposed by Figma, not chosen.

Unity, Blender and Godot impose the same shape for the same reason (an in-process
extension API plus a sandbox that cannot host a long-lived server). HLP is
therefore specified host-agnostically; `figma/*` methods are one binding.

## 2. Constraints

| # | Constraint | Consequence |
|---|---|---|
| C1 | Only the UI iframe has network access | The **host dials out**; the server listens |
| C2 | The iframe uses the browser `WebSocket` constructor | No custom headers → auth must ride in the URL |
| C3 | MCP servers speak JSON-RPC on stdio; **stdout is the transport** | All logging to stderr, without exception |
| C4 | Each MCP client spawns its own server process | Several servers, one host → must be disambiguated (§4) |
| C5 | Documents reach 100 MB+ | Payloads are forwarded, never parsed (§7) |
| C6 | Operations run for seconds to minutes | Deadlines must be extensible (§6) |
| C7 | A plugin reload drops the socket at any moment | Reconnect is normal, not exceptional |

## 3. Framing

**JSON-RPC 2.0**, one object per WebSocket text frame.

Chosen over a bespoke envelope because the message *kind* is then structural
rather than inferred: a response carries `result` xor `error`, a notification has
no `id`. A flat envelope with optional fields makes "is this progress or a
result?" a runtime guess on every frame.

Requests (server → host):

```json
{ "jsonrpc": "2.0", "id": 42, "method": "figma/getDocument", "params": { "depth": 3 } }
```

Responses (host → server) — exactly one of `result` / `error`:

```json
{ "jsonrpc": "2.0", "id": 42, "result": { "…": "opaque" } }
{ "jsonrpc": "2.0", "id": 42, "error": { "code": -32004, "message": "node not found" } }
```

Progress (host → server) — a notification, therefore no `id` field:

```json
{ "jsonrpc": "2.0", "method": "$/progress",
  "params": { "id": 42, "pct": 40, "note": "scanning text nodes" } }
```

`id` is a **monotonic `u64`** from a per-process counter, starting at 1. It is
opaque to the host and must be echoed exactly.

### Error codes

| Range | Meaning |
|---|---|
| -32700 … -32600 | JSON-RPC standard (parse / invalid request) |
| -32601 | Method not found — host does not implement it |
| -32602 | Invalid params |
| -32000 | Host threw; `message` carries the host's own text |
| -32001 | Deadline exceeded (synthesised server-side, never sent by the host) |
| -32002 | Host disconnected while the request was in flight |
| -32004 | Target not found (node, page, style) |

## 4. Session discovery

C4 means N servers contend for one host. Rather than have processes arbitrate
among themselves, **each server is independent and the user chooses**.

The picker lives in the host UI, which adds a constraint the obvious design
misses: the Figma iframe has **no filesystem access**, so it cannot read a
session directory. Discovery must therefore happen over the network the iframe
already has.

### Port range, first-free

A server binds the first free port in **51820–51839**. Twenty slots, no
contention, no arbitration: each process simply takes the next one. Exhausting
the range is a hard error, not a fallback to an ephemeral port — an
undiscoverable server is worse than a failed start.

### The `/hello` probe

`GET http://localhost:<port>/hello` requires no authentication and returns:

```json
{ "v": 1, "host": "figma", "pid": 31337,
  "label": "cursor — velocity-web", "started_at_ms": 1787740462000,
  "connected": false }
```

`connected` reports whether a host is already attached (§5), so the picker can
show which sessions are free rather than making the user discover it by
connecting. `started_at_ms` is epoch milliseconds as a 64-bit integer.

The host UI probes all twenty ports on open, lists what answers, and the user
picks one. A dead server is a port that does not answer — no stale state to
prune, no liveness bookkeeping.

`label` defaults to the parent process name plus the server's working directory,
which is what makes "which editor is this?" answerable at a glance.

### Session descriptors

A server also writes `${HLP_HOME:-~/.hostlink}/figma/sessions/<pid>-<port>.json`
with the same fields plus `port`, `0600` where the platform supports it, removed
on clean shutdown. This is for CLI tooling and debugging only — **the host UI
never reads it**, and nothing in the protocol depends on it.

A killed process never gets to remove its own descriptor, so each server prunes
on startup. Staleness is decided by **whether anything still answers on the
recorded port**, not by whether the pid exists: pids are recycled, ports are what
callers actually reach, and a descriptor naming the port we just bound is stale
by definition. A descriptor bearing the running process's own pid is left alone.

Consequences of the whole arrangement:

- No leader, no follower, no takeover race, no inter-process RPC hop.
- With several editors open, routing is **explicit and visible** instead of being
  decided by whoever won a port race.
- A dead server is a missing list entry, not a proxy timeout.

## 5. Connection and authorization

The host connects to:

```
ws://localhost:<port>/link?v=1
```

`v` is the protocol major version; an unknown value is refused with 400.

Origin is **not** checked: the iframe is a `data:` URL and sends `Origin: null`.

The host addresses the server as `localhost` rather than by IP, because Figma's
manifest rejects IP literals in `allowedDomains`. `localhost` resolves to `::1`
before `127.0.0.1` on most systems, so the server binds **both loopback
families** on its port rather than relying on the client to fall back.

### The threat model, stated honestly

On loopback there is no secret to protect: any local process can already probe
the range, and a token in a URL would not change that. What authorization means
here is that **the user picked this session in the Figma UI** — a deliberate
gesture, made in the host, that no other local process can perform on their
behalf. That gesture is the authorization boundary, and it is strictly more than
an unauthenticated always-on socket provides.

When `--bind` names a non-loopback address the calculus inverts, so a token is
then **required**: 32 bytes from a CSPRNG, hex-encoded, printed at startup and
carried as `?token=…`, compared in constant time, mismatch refused with 401
before the handshake completes. The server warns loudly in this mode.

At most one host connection is served per process. A second connection replaces
the first (a plugin reload is indistinguishable from this, per C7). In-flight
requests belonging to the replaced connection fail with -32002.

## 6. Deadlines

Each request carries a deadline, default 30 s, per-method overridable (document
reads default to 60 s).

A `$/progress` notification for an in-flight `id` **resets that request's
deadline** to the full interval. A host performing a long traversal therefore
stays alive by reporting progress, and a host that has genuinely hung still
times out. Progress for an unknown `id` is ignored — the request has already
completed or expired, and this is a normal race, not an error.

On expiry the server answers the MCP caller with -32001 and drops the pending
entry. A late response is discarded.

## 7. Payload handling

`result` is **never deserialized by the server.** It is captured as a raw JSON
fragment and forwarded to the MCP client verbatim.

The server parses only the envelope: `jsonrpc`, `id`, `method`, and which of
`result`/`error` is present. A 100 MB document therefore costs one buffer and one
move, not an object graph.

This is a load-bearing decision, not an optimisation: it is what makes the memory
profile flat with respect to document size, and it must survive refactors.

## 8. Method namespace

`<host>/<operation>`, lowerCamelCase after the slash: `figma/getDocument`,
`figma/setFills`. Operation names track the host's own API vocabulary — Figma's
`node.fills` gives `figma/setFills` — so the surface stays predictable and
carries no naming invention of its own.

`$/…` is reserved for protocol-level notifications (`$/progress` today).

Tier A implements: `getDocument`, `getNode`, `getSelection`, `getStyles`,
`getMetadata`, `getPages`, `getVariableDefs`, `getScreenshot`.

## 9. Versioning

`v` in the connect URL is the major version. Additive changes (new methods, new
optional params) do not bump it. Removing or re-typing a field does.

The server refuses a major it does not implement rather than negotiating down;
both halves ship together and version skew means a stale plugin, which should
fail loudly.

---

## Appendix A — provenance

HLP was specified from the constraints in §2 before implementation began. The
plugin-relays-to-external-server shape is imposed by the Figma plugin sandbox and
is long-established prior art in this space, notably
`grab/cursor-talk-to-figma-mcp` (March 2025, MIT).

Where this protocol makes a free choice it diverges deliberately from existing
implementations, on the merits recorded above: JSON-RPC 2.0 framing rather than a
flat envelope (§3), numeric ids rather than timestamp strings (§3), user-selected
session discovery rather than leader/follower election (§4), token auth rather
than an unauthenticated loopback socket (§5), and opaque payload forwarding
rather than full deserialization (§7).

No source from another implementation was consulted while writing this document
or the code implementing it.
