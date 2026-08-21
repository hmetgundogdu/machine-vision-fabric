# MVF worker protocol (control plane)

Language-agnostic contract between the engine (parent) and an out-of-process module
(child), spoken over the child's **stdio** — local only, **no network**. One JSON object
per line (newline-delimited). Targets: Python, Node.js, .NET.

Payload bytes are NOT part of this contract — they live in the **shared-memory data plane**. The
engine sets the child's `MVF_ARENA_PATH` env var to a memory-mapped file; a payload's `shm` handle
`{offset}` points at a slot. Each slot is `[descriptor header | payload bytes]`: the child reads the
**typed descriptor** (media type, dtype, shape, length — see `PayloadDescriptor` /
`docs/data-plane-design.md`) from the header at `offset`, then reads the payload at
`offset + 192` **in place** (zero copy). There is **no base64**; bytes never travel inline.

## Messages

Child → engine, on start (handshake). Ready immediately (no warmup):
```json
{"type":"hello","protocol":1,"moduleId":"py.brightness-classifier","capability":"classifier"}
```

Child → engine, when the module warms up asynchronously (loads a model, connects a device): the
`hello` says `"ready": false`, the child warms up, then signals `ready` (sd_notify `READY=1` style).
The engine waits for `ready` bounded by its **startup budget** — a slow warmup is a *startup* concern,
not a liveness failure, so it must not be mistaken for a hang. Absent/`true` `ready` = ready now.
```json
{"type":"hello","protocol":1,"moduleId":"py.warmup-classifier","capability":"classifier","ready":false}
{"type":"ready","moduleId":"py.warmup-classifier"}
```

Child → engine, optional `features`: capabilities beyond the base protocol that the engine may use with
this child. Absent or empty = base protocol only. Advertised per child rather than carried by the
`protocol` number so a new capability never breaks an older module, and because the engine must not send
a message this child would silently ignore. Currently defined: `configure` (see below).
```json
{"type":"hello","protocol":1,"moduleId":"py.rotary-inspect","capability":"processor","features":["configure"]}
```

Engine → child, run one node cycle:
```json
{"type":"execute","id":1,"frame":{"cameraId":"cam1","sequence":42,"contentType":"image/bmp","shm":{"offset":0}}}
```

Child → engine, result (classifier capability):
```json
{"type":"result","id":1,"classification":{"label":"black","measurement":3.2,"unit":"mean-byte","details":"n=64"}}
```

Engine → child, run one cycle (processor/transformer capability): same as `execute` plus a
pre-reserved output slot the child writes its new frame into (`[descriptor | payload]`, payload ≤
`capacity`). The child never allocates.
```json
{"type":"execute","id":1,"frame":{"cameraId":"cam1","sequence":42,"contentType":"image/bmp","shm":{"offset":0}},"out":{"offset":8388608,"capacity":8388416}}
```

Child → engine, result (processor capability) — a new frame in the output slot, or `null` to drop:
```json
{"type":"result","id":1,"frame":{"shm":{"offset":8388608}}}
```

Engine → child, run one cycle (analyzer capability): same as the processor form, because an analyzer may
optionally emit a derived frame. The child may also return a classification and structured inference
metadata in the same reply:
```json
{"type":"execute","id":1,"frame":{"cameraId":"cam1","sequence":42,"contentType":"image/bmp","shm":{"offset":0}},"out":{"offset":8388608,"capacity":8388416}}
```

Child → engine, result (analyzer capability) — any combination of a frame, a classification, and JSON
metadata:
```json
{"type":"result","id":1,"frame":{"shm":{"offset":8388608}},"classification":{"label":"ok","measurement":127.5,"unit":"mean-byte","details":null},"value":{"mean":127.5,"label":"ok"}}
```

Engine → child, apply the node's config to the **running** module. Sent once after `ready` (before the
first `execute`) when the node declares a config, and again whenever an operator edits one of its
`bindings`. The child applies it and replies `configured`:
```json
{"type":"configure","id":7,"config":{"acceptMinDeg":75,"acceptMaxDeg":105}}
{"type":"configured","id":7}
```
Only sent to a child whose `hello` listed `configure` in `features` (see below) — the SDKs' dispatch
loops *ignore* an unknown message type rather than refusing it, so an engine that guessed would wait for
a reply that never comes. A child that declines answers `error`; the engine then closes and reopens the
node, which is what it did for every edit before this message existed. Without it, a config on an
out-of-process node went nowhere: the engine resolved it, type-checked it, stored it and offered it as a
live tunable, and the worker never saw a byte of it.

Engine → child, capture durable state at a cycle boundary (resume-after-crash). The child writes its
serialized state into the reserved slot and replies `state`, or `{"empty":true}` when stateless:
```json
{"type":"checkpoint","id":1,"out":{"offset":8388608,"capacity":8388416}}
{"type":"state","id":1}
```

Engine → child (usually after a restart), restore previously captured state from a slot; the child
rehydrates its external resources and replies `restored`:
```json
{"type":"restore","id":1,"shm":{"offset":0}}
{"type":"restored","id":1}
```

Child → engine, failure for a request:
```json
{"type":"error","id":1,"message":"..."}
```

Child → engine, diagnostics (the engine forwards these upstream — CLI dashboard / stderr):
```json
{"type":"log","level":"info","message":"..."}
```
The engine also forwards the child's stderr lines (level `stderr`), so a traceback the module never
wrapped in a `log` still reaches the operator. `level` is `debug`/`info`/`warn`/`error`.

Engine → child, shutdown (engine then closes stdin):
```json
{"type":"shutdown"}
```

## Rules
- Every `execute` has an `id`; the matching `result`/`error` echoes it.
- `log` lines may appear at any time and are not responses.
- The child must `hello` before the engine sends any request.
- If the `hello` has `"ready": false`, the child must send `ready` (or exit) before the engine's startup
  budget elapses; the engine sends no request until then. `log` lines may precede `ready`.
- The engine only sends a message a child's `features` cover. A child must ignore an unknown message
  type rather than exiting, and must not rely on the engine sending anything it did not advertise.
- Flush after every line so the parent reads promptly.
