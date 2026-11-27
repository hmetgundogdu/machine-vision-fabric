# MVF worker protocol v2 (control plane, #32)

v2 replaces v1's JSON lines with length-prefixed binary frames on the same channel: the child's
inherited stdio pipes. Payload bytes still never travel here, only arena offsets. v1
([`README.md`](README.md)) stays supported: a child that does not speak v2 is served v1, and a
host can keep every child on v1 (`MVF_PROTOCOL=1`). Reference implementations: `rust/crates/mvf-ipc`
(framing, bodies, the fixture child) and `rust/crates/mvf-worker` (host). SDKs:
`src/sdk/python/mvf_sdk` (Python), `src/sdk/cpp` (C++) and `src/sdk/dotnet/Mvf.Worker` (.NET).

## Negotiation

1. **The host offers v2 in the child's environment.**
   - `MVF_PROTOCOL=2`;
   - `MVF_SESSION=<per-launch token>`;
   - `MVF_GENERATION=<node generation>`.

   For an OCI node the offer goes into the bundle's environment; the channel is the container's
   stdio, inherited from runc.

   A launch that offers only v1 says `MVF_PROTOCOL=1`, because a child inherits the host's
   environment. An SDK takes the offer only when `MVF_SESSION` is set too. So a stray
   `MVF_PROTOCOL=2` in a user's environment never switches a module that talks to a v1-only host.
2. **A v2 child answers with a binary `hello` frame** that carries the token. A v1 child ignores
   the offer and writes its JSON `hello` line. The host tells them apart by the first byte (`M`
   or `{`).
3. **Launch binding.** A `hello` without the launch's token is refused (`unauthorized peer`). The
   claimed version must match the framing: a JSON hello that says 2 is refused.

## Channel conventions for SDKs

- **Protect the channel.** The SDK keeps private duplicates of fd 0 and fd 1 for the protocol,
  then points fd 1 at stderr and fd 0 at the null device. A stray `print()` or a native library
  writing to stdout then lands in the logs instead of corrupting the protocol.
- **Logs go to stderr** as one JSON object per line: `{"level": "info", "message": "..."}`.
  Plain stderr lines are logged as `stderr`. Logs never use the control channel.
- **Per platform:**
  - On Windows, set the protocol descriptors to binary mode (no CRLF translation), make them
    non-inheritable, and also repoint the Win32 standard handles (`SetStdHandle`). Code that
    writes through the handles rather than the C runtime then lands in the logs too.
  - On Unix, make the duplicates close-on-exec, so processes the module starts do not hold the
    channel open.
  - A managed runtime must also redirect its own console writer. In .NET that is
    `Console.SetOut` onto stderr: the runtime's `Console.Out` opens its own copy of stdout.
- **Read whole frames.** Buffer the input and parse frames from the buffer. A request then
  costs one `read`.

## Frames

Each frame is a 24-byte header followed by a body. All integers are little-endian.

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | magic `MV` |
| 2 | 1 | version = 2 |
| 3 | 1 | kind |
| 4 | 2 | flags (kind-specific) |
| 6 | 2 | reserved (0) |
| 8 | 8 | id: correlation id (0 = unsolicited) |
| 16 | 4 | generation |
| 20 | 4 | body length (at most 1 MiB) |

**Generation.** Every request carries the node generation. A reply must carry the generation of
its request; a reply with another generation is refused (`stale generation`).

| Kind | Direction | Body |
|---|---|---|
| 1 hello | child → host | JSON `{protocol: 2, moduleId, capability, ready, features, session}` |
| 2 ready | child → host | JSON (optional) |
| 3 execute | host → child | binary, see below |
| 4 result | child → host | binary, see below, plus an optional JSON tail |
| 5 error | child → host | JSON `{message}`; `id` names the failed request |
| 6 configure | host → child | JSON config |
| 7 configured | child → host | empty |
| 8 checkpoint | host → child | `offset u64, capacity u64` |
| 9 state | child → host | empty; flags bit 0 = no state |
| 10 restore | host → child | `offset u64` |
| 11 restored | child → host | empty |
| 12 ping / 13 pong | host → child / reply | empty |
| 14 cancel | host → child | empty; `id` names the request (best effort) |
| 15 shutdown | host → child | empty |

**`execute` body.**

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | input slot offset (`u64::MAX` for a source) |
| 8 | 8 | sequence |
| 16 | 8 | output slot offset (`u64::MAX` = none) |
| 24 | 8 | output slot capacity |
| 32 | 4 | deadline: the remaining budget in µs (0 = none) |
| 36 | 2 | camera id length |
| 38 | 2 | content type length |
| 40 | — | camera id bytes, then content type bytes |

**`result` body.**

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | flags: 1 frame written, 2 frame dropped, 4 source exhausted, 8 JSON tail follows |
| 4 | 4 | reserved |
| 8 | 8 | offset of the written frame |
| 16 | 8 | a source's sequence (0 = default) |
| 24 | — | JSON tail `{classification?, value?, cameraId?}` when flags bit 8 is set |

A processor's result is 24 bytes and contains no JSON.

## Semantics

- **Ordering and concurrency.**
  - Requests and replies are matched by `id`.
  - The host keeps one request in flight per child. Parallelism is several children (node
    `parallelism`), not several requests on one.
- **Deadlines.** The host enforces them. When the deadline elapses, the child is terminated and
  the node fails with `Timeout`; this covers a frame left half-written. The `deadline` field lets
  a child give up early.
- **Cancellation.** The host stops waiting at once, sends `cancel` and terminates the child.
  Single-threaded SDKs may ignore `cancel`; it arrives after their request finished.
- **Protocol violations.** The child is terminated at once and the node fails with `Protocol`.
  These are:
  - bad magic, wrong version or unknown kind;
  - a body over the limit (refused from the header);
  - a malformed body;
  - a stale generation;
  - a missing launch token.

  The host never tries to resynchronise on a broken stream.
- **Crashes.** An end of stream, inside a frame or not, is reported as the process exit with its
  code or signal.
- **No replay.** The protocol never resends a request. What happens after a crash is the node's
  failure policy. `onError: restart` re-runs the failed cycle on a fresh process: at-least-once
  for that cycle, so a node with external side effects must be idempotent per `sequence` or use
  `skip` / `fail`.
- **Health.** `ping`/`pong` is a liveness probe available to the host. Readiness stays `hello` +
  `ready`.
- **Logs.**
  - A bounded queue (4096 lines) feeds a delivery thread.
  - When the consumer is too slow, new lines are dropped and counted
    (`dropped N log line(s)`). Execution never waits for a log consumer.

## Compatibility and retirement of v1

- **v2 is offered by default.** `MVF_PROTOCOL=1` on the host keeps every child on v1, as a
  rollback switch.
- **Who speaks v2:** the Python, C++ and .NET worker SDKs and the Rust fixture. Each one still
  speaks v1 when v2 is not offered.
- **Who speaks only v1:** the shipping .NET engine's worker host
  (`src/hosting/Mvf.Hosting.Worker`). It never offers v2, so the SDKs answer it in v1.
- **Gate for retiring v1:**
  - every shipped SDK speaks v2 (met);
  - every e2e suite has passed on v2 on Linux, Windows and macOS for one release;
  - no v1-only host or module is in use. The .NET engine's host is one.

  Until then the host and the SDKs keep both.
