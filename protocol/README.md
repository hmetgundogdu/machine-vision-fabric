# MVF worker protocol (control plane)

Language-agnostic contract between the engine (parent) and an out-of-process module
(child), spoken over the child's **stdio** — local only, **no network**. One JSON object
per line (newline-delimited). Targets: Python, Node.js, .NET.

Big frame/tensor data is NOT part of this contract long-term — it belongs to the
shared-memory data plane (M2). A frame payload is carried one of two ways:
- **shared memory (M2, preferred):** the engine sets the child's `MVF_ARENA_PATH` env var to a
  memory-mapped file. The frame's `shm` handle `{offset,length}` points into it; the child maps the
  same file and reads the bytes **in place** (no copy off the pipe, no base64). See
  `docs/data-plane-design.md`.
- **inline (M1 fallback):** `dataBase64` carries the bytes when no arena is present, or when a frame
  is larger than a slot.

## Messages

Child → engine, on start (handshake):
```json
{"type":"hello","protocol":1,"moduleId":"py.brightness-classifier","capability":"classifier"}
```

Engine → child, run one node cycle (shared-memory frame):
```json
{"type":"execute","id":1,"frame":{"cameraId":"cam1","sequence":42,"contentType":"image/bmp","length":64,"shm":{"offset":0,"length":64}}}
```

Engine → child, run one node cycle (inline fallback):
```json
{"type":"execute","id":1,"frame":{"cameraId":"cam1","sequence":42,"contentType":"image/bmp","length":64,"dataBase64":"<...>"}}
```

Child → engine, result (classifier capability):
```json
{"type":"result","id":1,"classification":{"label":"black","measurement":3.2,"unit":"mean-byte","details":"n=64"}}
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
