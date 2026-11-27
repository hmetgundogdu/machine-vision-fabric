# ADR-0006 — Worker control protocol v2: binary frames on the inherited pipes

- **Status:** Accepted for the research branch (#32). Not a migration decision.
- **Date:** 2026-11-27
- **Issues:** #32, part of #10. Related: #28 (runtime hardening), #23, #21 (the Event Fabric is
  explicitly out of scope).
- **Evidence:** [stage 21](stages/21.md), `crates/mvf-ipc/tests/bench.rs`,
  `crates/mvf-ipc/tests/faults.rs`. Spec: [`protocol/worker-v2.md`](../../protocol/worker-v2.md).

## Context

Workers and OCI nodes speak protocol v1 with the engine: JSON lines on stdio. Payloads already
travel in shared memory, so this channel carries only control messages. v1 had four weaknesses:

- Any byte a module or a native library wrote to stdout landed on the control channel.
- Logs and control shared that channel.
- Every message cost a JSON encode and parse on both sides.
- Every reply cost two thread hops on the host: a reader thread, then a channel handover.

## Measured (20,000 `execute` → `result` round trips; Apple M-series and a 10-vCPU aarch64 Linux VM)

| Case | macOS p50 / p99 µs | Linux p50 / p99 µs | Context switches per round trip | Child CPU per msg, macOS / Linux |
|---|---|---|---|---|
| Rust child, v1 JSON, reader thread (v1 host) | 9.4 / 17.2 | 27.9–39.2 / 44.1–51.3 | 2 | 5.1 / 6.2–7.6 µs |
| Rust child, v2 binary, reader thread | 6.9 / 10.0 | 27.5–34.3 / 45.5–51.0 | 2 | 2.9 / 5.4–6.0 µs |
| **Rust child, v2 binary, direct read** | **4.5 / 7.5** | **18.2–22.6 / 25.8–31.3** | **1** | 2.5 / 5.1–5.9 µs |
| Rust child, v2 binary, Unix socket pair, direct read | 5.1 / 8.2 | 20.7–25.4 / 31.2–33.0 | 1 | 2.8 / 6.2–7.0 µs |
| Python child, v1 JSON, reader thread | 11.0 / 18.0 | 36.7–45.8 / 50.3–56.3 | 2 | 9.2 / 11.3–13.4 µs |
| **Python child, v2 binary, direct read** | **6.8 / 9.2** | **19.9–24.7 / 27.1–34.0** | **1** | 5.3 / 6.9–8.1 µs |

Linux ranges span two runs on the VM.

Through the real engine (8×8 frames, cycle time only):

| Node | macOS v1 → v2 | Linux v1 → v2 |
|---|---|---|
| Rust analyzer | 12.7 → 9.8 µs/frame | 39.3 → 27.9 µs/frame |
| Python transformer | 24.2 → 19.3 µs/frame | 47.9 → 14.3 µs/frame |

## Decision

1. **Transport: keep the inherited stdio pipes.**
   - There is no endpoint, so there is no peer to authenticate beyond the launched process.
   - They work through `runc` unchanged and on Windows, with no named pipe and no socket file.
   - A Unix socket pair measured no faster (slightly slower in every run), and would need a
     different mechanism on Windows.
   - The benefit sockets would bring is descriptor passing (`SCM_RIGHTS`, e.g. memfd arenas).
     That can be negotiated later as a feature.
2. **Framing: a 24-byte binary header and a bounded body.**
   - Fixed binary bodies on the hot path: `execute`, `result`, `checkpoint`, `restore`.
   - JSON only where content is open-ended: `hello`, config, errors, classification and value
     tails.
   - This cut child CPU per message by 35–45%.
3. **Schema: our own fixed layouts, not Protobuf, FlatBuffers or Cap'n Proto.** Every runtime
   option would add a dependency to SDKs that are dependency-free today:
   - Python `protobuf` or `flatbuffers`, or `pycapnp` (a C extension);
   - Google.Protobuf for C#;
   - generated code for C++.

   The hot-path messages are a handful of fixed integers, which `struct` (Python),
   `BinaryPrimitives` (C#) and `memcpy` (C++) decode at once. Versioning is the frame version plus
   the `features` list (unchanged from v1). Revisit if control messages grow complex.
4. **Receive path: direct read on Unix.**
   - A non-blocking pipe, `poll` and an incremental frame buffer, on the calling thread.
   - One context switch per reply instead of two. A half-written frame is bounded by the
     request's deadline.
   - Windows keeps a reader thread: anonymous pipes cannot be polled, and overlapped named pipes
     would be a second transport. This is measured in CI.
5. **Logs off the control path.** Logs go to stderr as JSON lines, through a bounded queue to a
   delivery thread that drops and counts lines when the consumer lags. The SDK moves stdout onto
   stderr, so the channel cannot be polluted.
6. **Semantics are explicit:**
   - correlation ids and generation-stamped frames (stale replies are refused);
   - a per-launch session token (launch binding);
   - a 1 MiB body limit checked from the header;
   - violations terminate the child at once and are never resynchronised;
   - deadlines enforced by the host, with the remaining budget carried in `execute`;
   - best-effort `cancel` and `ping`/`pong`;
   - no protocol-level replay. Restart semantics stay the node's failure policy: at-least-once for
     a crashed cycle.
7. **Compatibility.**
   - v2 is offered by default; a v1 child is detected by its first byte and served v1;
     `MVF_PROTOCOL=1` keeps everything on v1.
   - Python and the Rust fixture speak v2 now. C++ and .NET keep v1 until ported (part 2).
   - v1 is retired only behind the gate in the spec.

## Consequences

- **Every existing Python worker and OCI test now runs on v2,** with no test change except the
  fixture that deliberately speaks an unsupported version. That covers checkpoint/restore,
  restart, live replacement and rollback, private arenas, warm spares, display, storage and the
  FTP source.
- **Control overhead roughly halves** where it dominates: small frames, high rates, many nodes.
  For frames that take milliseconds to process it stays negligible either way. The main gains are
  reliability and diagnosability.
- **Not done here:**
  - C++ and .NET v2;
  - descriptor passing;
  - several requests in flight per child;
  - a liveness probe scheduled by the engine (`ping` exists);
  - Windows direct reads;
  - measurements on constrained edge hardware. Here only an M-series machine and a VM were
    measured.

## Threat model

- **Protects against:**
  - a module or library corrupting the channel by writing to stdout;
  - a confused or substituted process answering for a launch it does not belong to (token);
  - stale replies across generations;
  - oversized or malformed frames exhausting memory or desynchronising the host;
  - a log flood stalling execution.
- **Does not protect against:**
  - a malicious process of the same user, which can read the token from the child's
    environment;
  - a compromised child sending well-formed lies (a sandbox limits what it can reach, the
    protocol does not);
  - side effects repeated by a restart policy the node chose.
