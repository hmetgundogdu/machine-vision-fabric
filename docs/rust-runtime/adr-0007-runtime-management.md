# ADR-0007 — Runtime management: a read-only local feed first, remote access through a separate relay

- **Status:** Accepted for the research branch (#33). All three parts are implemented: part 1
  (local observation), part 2 (commands, roles, audit, the TLS relay) and part 3 (previews,
  package upload and package signatures). Measurements on edge hardware are not done (see
  Plan). Not a migration decision.
- **Date:** 2026-11-27
- **Issues:** #33, part of #10. Related: #32 (worker IPC, which management never exposes), #23
  (node networking), #30, #31. Separate from #21 (Event Fabric).
- **Evidence:** [stage 24](stages/24.md) (part 1), [stage 25](stages/25.md) (part 2),
  [stage 26](stages/26.md) (part 3), [stage 27](stages/27.md) (package signatures),
  [stage 28](stages/28.md) (previews in the pipelined executor),
  [stage 31](stages/31.md) (the TUI client, activity sampling, #35),
  `crates/mvf-management/tests/{manage,commands}.rs`, `crates/mvf-remote/tests/remote.rs`,
  `crates/mvf-cli/tests/cli.rs`. Spec: [`protocol/management-v1.md`](../../protocol/management-v1.md).

## Context

Operators need to see a running runtime: its graph, node state, latency, errors, replacement
progress and logs. Later they also need to drive it, locally or from another machine on the LAN:
deploy prepared artifacts, replace and roll back nodes, and see previews. The constraints:

- The edge runtime is autonomous. There is no mandatory central server, broker or cloud, and the
  engine stays the only scheduling authority.
- **Production first.** A slow, disconnected or malicious client, or a crash of the management
  code, must never stall or stop execution.
- **Secure by default.** No unauthenticated endpoint, no default network listener. Configuration
  and images are protected. Read-only access is separate from control.

## Decision

1. **Process boundary.**
   - **Observation and the local endpoint run inside the runtime process, on their own threads.**
   - The engine thread only updates counters it owns and publishes them with a non-blocking lock
     attempt. Readers copy reference-counted, pre-serialised lines out of a bounded ring. Each
     client has its own thread, and a panic there is caught.
   - **Remote access (part 2) is a separate, optional relay process** (`mvf-remote`). It connects to the
     local endpoint as an ordinary client and serves TLS on an explicitly configured interface.
     Network-facing code and the TLS stack then never run in the production process. A relay
     crash or compromise reaches the runtime only through the local API's own checks, and the
     relay can run with fewer privileges.
   - **Rejected:**
     - an in-process TLS server: network attack surface inside the process that drives machines;
     - a management daemon that owns the runtimes: a central authority on the edge box, against
       the autonomy rule.
2. **Wire protocol: JSON lines, versioned by `hello`.** A snapshot gives the current state with
   its sequence number. After it comes one strictly increasing stream of entries: node deltas
   with absolute values, engine events, logs and status. Gaps are explicit, and a gap is followed
   by a fresh snapshot. Reconnecting resumes from `last + 1`.
   - **Why not gRPC/Protobuf now:** this is telemetry at 10 Hz. Encoding is not the cost (about
     20 KiB/s per client, measured), while HTTP/2 and Protobuf would add dependencies to the
     runtime and to every client.
   - JSON lines are read by any language's standard library, as the Python client shows, and
     carry over a WebSocket or TLS relay unchanged.
   - Previews (part 3) carry pixels and will get a binary side channel. The choice will be
     measured then.
3. **Local authentication by OS identity.**
   - **Unix:** a 0700 runtime directory that must belong to the user, a 0600 socket, and a
     peer-uid check on every connection.
   - **Windows:** a named pipe with an explicit DACL (current user and LocalSystem; the default
     DACL lets Everyone read). Remote pipe clients are rejected, and the first instance is
     exclusive, so the name cannot be squatted.
   - In part 1 the owner may observe and nothing else exists.
4. **Authorization (part 2, built).**
   - **Roles:** `observer` (topology, state, events, logs) and `operator` (plus commands). The
     local owner is an operator. A `previewer` role comes with previews in part 3.
   - **Relay clients:** they authenticate with TLS 1.3 client certificates from the relay's own
     CA. The role is not written into the certificate. It comes from an access list on the edge
     box, keyed by certificate fingerprint and re-read for every connection, so a role change or
     a revocation needs neither a new certificate nor a relay restart.
   - **Two checks:** the relay refuses an observer's command itself. It also speaks
     `onBehalfOf` the principal, so the runtime enforces the role again and audits the real name.
   - **Commands** are update-plan steps validated exactly like a plan file, applied by the
     engine at a cycle boundary; `stop` goes through the run's cancellation token. Each command
     has an idempotency key (a repeat is never re-sent), a deadline and an explicit result
     (`applied`, `rejected`, `pending` with a sequence, or `refused`). Every one is appended to an
     audit log that is synced to disk.
   - No shell, no filesystem paths outside the package, and no replay of a command.
   - **TLS stack:** rustls with the ring provider, TLS 1.3 only, in the relay process alone. The
     runtime binary links no TLS.
5. **Telemetry budget.**
   - **Engine thread:** about 15 ns per node event (a histogram increment and counters).
   - **Publication:** every 100 ms.
   - **Ring:** 4096 entries.
   - **Logs:** 200/s sustained and bursts of 1000. Excess is counted, never queued.
   - **Snapshots:** the last 50 logs and events.
   - **Clients:** at most 8, with requests of at most 64 KiB. On Unix a write blocked for 2 s
     drops the client.
   - The runtime keeps nothing for a client beyond the shared ring.
6. **Privacy.** Config values never leave the runtime through management; the topology names
   editable fields only. Logs are forwarded as modules write them, so modules must not log
   secrets.
7. **Previews (part 3, built).**
   - **Opt-in:** a node is previewed only if it is named when the runtime starts
     (`--preview NODE`), and only for a `previewer` or `operator`.
   - **Sampling:** the engine thread samples at most `max_fps` per node, through a new
     `Observer::wants_outputs`/`outputs` pair. It passes a shared lease through a single-slot
     queue and never waits. A separate thread downscales nearest-neighbour to at most 160×120 and
     drops the lease at once.
   - **Encoding:** PNG with uncompressed deflate (no codec dependency), base64 in the JSON line.
     That costs a third more bytes than binary, but keeps one protocol for the CLI, the relay and
     the Python client. Each viewer has a byte budget (256 KiB/s by default), and a preview that
     does not fit is skipped, never queued.
   - **Retention:** only the latest preview per node, in memory. It is never in the ring,
     snapshots, the audit log or on disk. Opening a preview is audited.
   - **Pipelined executor ([stage 28](stages/28.md)).** The observer lives on the driver thread,
     so sources are sampled as in the serial executor. For a mid-graph node the driver asks once
     per cycle while no sample of it is in flight, and sets a per-node flag. The node's stage
     thread clears it after its next execution and sends that execution's outputs (shared
     leases) back on the event channel it already uses for node events. So a stage never
     waits, and has at most one sample per node in flight. An unarmed stage pays one relaxed
     atomic load per execution.
     - **Rejected:** a thread-safe observer called from the stage threads (every observer would
       need locking for a feature most runs leave off); sending every output to the driver
       (a lease per frame held until the driver drains).
   - **Rejected:**
     - a binary side channel: a second framing through the relay for a stream capped at
       kilobytes per second;
     - JPEG: a codec dependency in the runtime.
8. **Package upload (part 3, built).**
   - **Transfer:** chunked, resumable from `upload-status` after a dropped connection or a
     restart. Each file has a declared SHA-256, checked on arrival and again at staging.
   - **Staging:** names are confined, sizes and counts capped, and staging is one atomic rename
     into `<pipeline dir>/.mvf/staged/<package>`. A staged package is immutable: a new version is
     a new name.
   - **Activation:** an ordinary `replace` naming the staged contract, so it gets the same
     validation, probation and `rollback` as any replacement. Upload never activates anything by
     itself.
   - **Signatures (built, [stage 27](stages/27.md)).** With `--trust-signer` keys configured,
     `stage` requires an Ed25519 signature by one of them over a versioned line format: the
     package name and every file's SHA-256, sorted. A signature over other files or another
     name, an untrusted key and a missing signature are all refused. The signature is kept
     beside the manifest. Without trusted keys, packages stage unsigned, and trust rests on who
     may upload.
     - **Two implementations on purpose.** The runtime verifies with `ed25519-dalek` (pure Rust,
       so the runtime still has no C crypto, and the Windows cross-check covers it).
       `mvf-remote sign` signs with `ring`, which the relay already has. The tests check that
       the two agree.
     - **Rejected:** signatures embedded in the package (they would change the files they
       sign); a CA chain for signers (an access list of keys is enough for a fleet the operator
       configures, as with `access.json`).

## Measured (part 1; 200,000 cycles of 4 in-process nodes, median of 3 runs)

| | macOS µs/cycle | Linux VM µs/cycle |
|---|---|---|
| management off | 1.106 | 1.106 |
| on, no client | 1.161 | 1.146 |
| on, 2 clients (≈20 KiB/s each) | 1.169 | 1.184 |
| on, 1 stuck client | 1.175 | 1.178 |

That is about 0.06 µs per cycle for four node events, which is invisible next to a worker round
trip (5–25 µs, ADR-0006) or a millisecond-scale inspection step. CPU per cycle tracks wall time,
because the extra work is on the engine thread itself.

Part 2, the relay (median of 2000 pings; one remote watcher on a 10 ms publication interval):

| | macOS | Linux VM |
|---|---|---|
| ping round trip, local endpoint (p50 / p99) | 10 / 14 µs | 12 / 28 µs |
| ping round trip through the relay, TLS 1.3 (p50 / p99) | 24 / 75 µs | 65 / 83 µs |
| relay CPU while streaming ≈70 KiB/s to a client | 1.4 % of one core | 1.6 % of one core |

The runtime's own cost is unchanged by the relay: to the runtime it is one more local client.

Part 3 (release builds):

| | macOS | Linux VM |
|---|---|---|
| 50k cycles of 64×48 RGB, previews off / on (2 fps) | 10.1–10.8 / 10.3 µs/cycle | 11.55–11.66 / 11.73–11.74 µs/cycle |
| one preview of 1280×960 RGB → 160×120, on the preview thread | 0.70 ms, 77 KB on the wire | 0.58 ms, 77 KB |
| 32 MiB upload over the local endpoint (base64 chunks, digest checks, staging) | 81 MiB/s | 78 MiB/s |
| pipelined, 50k cycles of 64×48 RGB, previews of a source and a stage off / on (2 fps, wall) ([stage 28](stages/28.md)) | 12.22 / 12.21–12.26 µs/cycle | 12.53–12.83 / 12.50–12.96 µs/cycle |

## Threat model (parts 1–3)

- **Protects against:**
  - another local user reading or connecting (filesystem permissions, then the peer-uid check;
    on Windows, the DACL);
  - a planted endpoint (the runtime directory must be private and the user's own);
  - pipe-name squatting (exclusive first instance);
  - a client stalling the engine or other clients (non-blocking publish, per-client threads,
    write timeouts, bounded ring and budgets);
  - oversized or malformed requests;
  - configuration disclosure through the topology;
  - **remotely, through the relay:**
    - network eavesdropping and tampering (TLS 1.3);
    - certificates from another CA, or expired (handshake);
    - CA-signed certificates not admitted, or revoked (access list);
    - an observer issuing commands (refused twice);
    - command replay (idempotency keys);
    - an operator naming files outside the package (resolver confinement);
    - unattributed actions (audit under the remote principal);
    - a relay crash taking the run down (separate process; tested with SIGKILL);
  - **previews:** images of a node nobody opted in (refused), a viewer without the role (refused
    twice), and images lingering anywhere (nothing retained);
  - **uploads:**
    - a corrupted or swapped file (digest check, twice);
    - path traversal (confined names);
    - an oversized package (caps);
    - a package changed after staging (immutable);
    - an upload by anyone but an operator;
    - with trusted signers, a package nobody trusted signed, or one changed after signing,
      even when uploaded by an operator.
- **Does not protect against:**
  - a process of the same user: it is the operator, as with the worker protocol (ADR-0006);
  - an administrator or root;
  - secrets that modules themselves write to their logs;
  - an attacker who holds the CA key or an admitted operator's key. Keep `ca.key` offline and
    issue short-lived client certificates. Part 2 has no CRL or OCSP: revocation is the access
    list;
  - denial of service on the relay's port beyond its connection cap. Firewall it to the
    operators' network.

## Plan

- **Part 2** is done ([stage 25](stages/25.md)). `start` is not a command: a runtime that is not
  running has no endpoint, and starting one is the job of the host's service manager.
- **Part 3** is done ([stage 26](stages/26.md); signatures in [stage 27](stages/27.md);
  previews in the pipelined executor in [stage 28](stages/28.md)), except edge-hardware
  measurements (BLOCKED).
- **Part 3, as originally planned:**
  - opt-in previews with resolution, rate and bandwidth caps;
  - artifact upload with digest and signature policy, staged before activation and resumable
    after interruption;
  - measurements on constrained edge hardware.
