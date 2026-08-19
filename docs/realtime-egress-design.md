# Realtime Egress — design

> Milestone **EG** (`roadmap.md`). Lets a running pipeline *emit while it runs* so an external panel /
> studio / observer can watch it live — **without** ever touching the execution hot path.

## Why this is a seam, not a data edge

Per the **Telemetry Rule**, egress is **optional, non-blocking, best-effort**. It is *publish*, not a
data edge: it never participates in refcounts or ordering, and a slow or absent consumer must **drop**,
never stall the graph. Egress also *leaves the machine*, so — unlike the runtime's own data/control
plane, which is strictly local IPC (no network) — it is a **separate, opt-in adapter behind a seam the
core never references**, the same shape as `IDataPlane` / `IOutOfProcessModuleHost`. Loopback-first: the
runtime's no-network constraint is preserved because egress is not the runtime's transport.

## What is published

Two **stream kinds**, both as one framed binary record type:

- **Cycle** — one record at the end of each completed cycle (cadence: every accepted cycle). Small
  state: run id, cycle index, totals, accepted, elapsed. Source: `PipelineExecutionProgress`.
- **NodeTransition** — one record per node handoff. Node id, ports, duration, worker cpu/mem, and the
  emitted **`OutputFrameBytes`** (size). Source: `NodeExecutionEvent`. Optionally carries the **frame
  payload** itself (frame-data) — see below.

**Frame-state** is a NodeTransition/Cycle record with no payload. **Frame-data** is a NodeTransition
record whose payload is the emitted frame bytes, described by the arena's own `PayloadDescriptor`.

## Wire format (little-endian, framed, bulk)

```
[u32 recordLen]                          // bytes that follow
[u16 magic 'EG' 0x4745][u8 version=1][u8 streamKind]   // 0=Cycle 1=NodeTransition
[runId: 16 bytes]
[u32 cycleIndex][u32 seq]
--- NodeTransition only ---
[u16 nodeIdLen][nodeId utf8][u16 portLen][port utf8]
[i64 durationMicros][u8 flags: bit0 hasOutput, bit1 faulted]
[i64 outputFrameBytes]                   // -1 when none
[u8 hasPayload]
  if hasPayload==1: [PayloadDescriptor: 192 bytes, reused verbatim][payload bytes]
--- Cycle only ---
[u32 totalCycles][u32 acceptedCycles][u8 accepted][i64 elapsedMillis]
```

The `PayloadDescriptor` (`Mvf.Abstractions.PayloadDescriptor`, `HeaderSize = 192`) is **reused
verbatim** — it already carries dtype + shape + mediaType + length in fixed little-endian binary,
co-located with the arena bytes. Forwarding descriptor + bytes is near-zero-copy; the consumer wraps the
bytes as a native typed object (numpy / `Span<T>` / `TypedArray`) exactly as an in-graph consumer would.

## Hot-path guarantee

The engine calls `IEgressSink.TryPublish(...)` at the existing tap sites — it **enqueues into a bounded
ring and returns immediately**. Full ring ⇒ drop + bump a counter (best-effort). A background **drain**
thread serializes queued records into a pooled buffer and writes them to the transport. No subscriber ⇒
enqueue-then-drop, ~free. The executors are unchanged in structure: both already fire the observation
callbacks (`PipelineGraphExecutor.cs:442/541`, `PipelinedGraphExecutor.cs:376/449`); egress is an
additive call beside each, reusing data already built.

## Transports (selectable, loopback-first)

Every transport carries the *same* framed records.

| Transport | Role | Frame-data? |
|-----------|------|-------------|
| **TCP server** | native / Node consumers (bulk) | yes |
| **WebSocket server** | **browser** consumers (a browser can't open raw UDP/TCP) — WS binary frames | yes |
| **UDP multicast** | small **state** records, LAN fan-out | no (exceeds MTU; state-only) |

Config lives in **core** (`EgressOptions` on `PipelineExecutionOptions`): `Enabled`, `Transport`
(`Tcp|WebSocket|Udp`), `Port`, `Streams` (`State|Frame` flags), cadence. The CLI is one front-end that
sets it; a future host reads the same options. The socket impls live in `src/egress/Mvf.Egress`, which
core and engine never reference.

## Discovery (alive beacon)

When egress is active the edge periodically (~1 s) emits a small UDP **multicast** beacon to a
well-known group/port:

```
{ magic, version, edgeId (stable machine id), pipelineName, runId,
  transport, port, streams, status(running|paused), timestampUtc }
```

A viewer listens on the group and keeps a **live, self-expiring** list of streaming edges (an entry
disappears when its beacons lapse — the "alive" semantics), then connects to a chosen edge's TCP/WS
server or joins its UDP state stream. Hand-rolled minimal beacon (build-our-own ethos), not mDNS.
**Browser caveat:** a browser can't receive UDP multicast, so a browser viewer discovers via a known WS
URL or a small Node-side relay; native/Node viewers get full auto-discovery.

## Consumer / observer SDK

A **consumer** SDK, distinct from the module-author SDKs under `src/sdk/{dotnet,python,cpp}`:

- **Shared codec.** Encode lives once in `Mvf.Egress`; the .NET consumer SDK (`Mvf.Egress.Client`)
  decodes from the same codec + `PayloadDescriptor`. The TS codec is a pure `ArrayBuffer`/`DataView`
  port — isomorphic, identical in browser and Node.
- **TypeScript** (`mvf-egress-client`), dual entry: **Node** (UDP `dgram` discovery + TCP `net` / WS
  stream) and **browser** (WS stream; discovery via known URL/relay). Exposes a discovery observable
  (live edge list) + a stream client yielding typed `CycleRecord` / `NodeTransitionRecord` (payload
  wrapped as a typed array via the descriptor dtype/shape).
- **.NET** (`Mvf.Egress.Client`): discovery listener + stream client yielding the same typed records
  (payload as `Span<T>`).

## Delivery slices

0. ✅ Design doc (this) + roadmap flip.
1. ✅ Core seam (`EgressOptions`, `IEgressSink`) + state stream + `TcpServerEgressSink` + CLI `--egress` flags.
2. ✅ TypeScript consumer SDK (`clients/mvf-egress-client`, Node) + reference viewer (wire-format contract test).
3. ✅ Frame-data (bulk) over TCP — node-transition carries the `PayloadDescriptor` + bytes; the engine reads
   the frame only when a subscriber wants frames (`WantsFrames`), off the unwatched hot path.
4. ✅ Discovery beacon (`EgressBeacon`, UDP multicast) + WebSocket transport (`WebSocketEgressSink`) +
   browser SDK entry (`mvf-egress-client/browser`) + browser viewer.
5. ✅ UDP state transport (`UdpEgressSink`, multicast, state-only) + .NET consumer SDK (`Mvf.Egress.Client`:
   TCP / UDP / discovery clients + `EgressEdgeRegistry`) + a **separate-page** TUI egress view (open with
   `e`: transport, endpoint, streams, discovery, live subscribers / published / dropped).
