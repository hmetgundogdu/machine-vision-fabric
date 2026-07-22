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

Child → engine, on start (handshake):
```json
{"type":"hello","protocol":1,"moduleId":"py.brightness-classifier","capability":"classifier"}
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

Child → engine, optional diagnostics (engine may ignore/forward):
```json
{"type":"log","level":"info","message":"..."}
```

Engine → child, shutdown (engine then closes stdin):
```json
{"type":"shutdown"}
```

## Rules
- Every `execute` has an `id`; the matching `result`/`error` echoes it.
- `log` lines may appear at any time and are not responses.
- The child must `hello` before the engine sends any request.
- Flush after every line so the parent reads promptly.
