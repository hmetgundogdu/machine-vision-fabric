# Architecture Foundation

The canonical, high-level architecture of **MachineVisionFabric (MVF)**. For the
evolving plan and decision log see [roadmap.md](roadmap.md); deeper mechanics live in
the linked design docs at the end.

## 1. Purpose & deployment assumptions

MVF is an open-source, **edge-first** vision pipeline platform. It executes a strict,
schema-validated graph of nodes — cameras, transforms, AI inference, PLC/control,
storage — on the edge device itself.

Operating assumptions:

- Runs inside company networks on panel PCs, industrial PCs, and NUC-class devices.
- **Windows-first**, but the runtime and SDKs are cross-platform (Linux, macOS).
- Must keep running **without a central server**. A central system is optional and only
  ever *observes* (logs, health, inventory, optional telemetry) — it never owns execution.

## 2. Architecture at a glance

```
        edge device                                     optional, best-effort
┌──────────────────────────────┐                    ┌────────────────────────┐
│  MVF edge runtime            │  telemetry (WS) ──▶ │  central observer      │
│  ├─ sources / simulators     │  (non-blocking)     │  logs · health ·       │
│  ├─ typed graph executor     │                     │  inventory · pipeline  │
│  ├─ polyglot module hosts    │                     │  telemetry             │
│  ├─ AI inference (ONNX)      │                     └────────────────────────┘
│  ├─ PLC / control            │
│  └─ local persistence        │
└──────────────────────────────┘
```

The edge runtime is self-sufficient. Telemetry publishing is optional, best-effort, and
**never on the execution hot path**.

## 3. The strict typed graph

A pipeline is a directional, schema-validated graph. The mental model is:

```
NODE(typed output port) ──▶ NODE(typed input port) ──▶ NODE(typed output port)
```

Two edge types are **first-class and never collapsed**:

- **data edge** — frame / tensor / payload transfer.
- **control edge** — decisions and branch selection (e.g. a PLC presence signal, a
  classifier's class used by `if`/`switch`).

Typing is enforced at authoring time *and* re-validated at runtime, not just in a UI.
See [pipeline-graph-foundation.md](pipeline-graph-foundation.md).

## 4. Typed payloads & the data plane

Payloads are **typed and byte-based**. They live in a shared-memory **arena** as
`[descriptor header | payload bytes]`; the self-describing descriptor carries media type,
element type, shape and strides. Modules read and write payloads **in place, zero-copy** —
there is no base64 and bytes never travel inline on the control channel.

See [data-plane-design.md](data-plane-design.md).

## 5. Node contract & the polyglot module protocol

Out-of-process modules speak one language-agnostic protocol:

- **control plane** — newline-delimited JSON over the module's stdio (handshake, execute,
  checkpoint/restore, readiness, shutdown). See [`protocol/README.md`](../protocol/README.md).
- **data plane** — the shared-memory arena above.

Because the contract is language-agnostic, modules can be authored in **.NET, Python, or
C++** interchangeably (`src/sdk/{dotnet,python,cpp}`). In-process .NET integration modules
are also supported via `Mvf.Sdk` base classes. See
[integration-sdk-strategy.md](integration-sdk-strategy.md) and
[sdk-quickstart.md](sdk-quickstart.md).

## 6. Node categories & engine primitives

Work nodes come from SDK modules; **flow-control primitives are owned by the engine** (they
define execution semantics, not device behaviour):

- Module categories: `source`, `compute` (processor), `classify`, `control`/`gate`, `sink`.
- Engine primitives: `if`, `switch`, `fork`, `loop`, plus typed `value` / `select`.

`loop` is the graph's **iteration authority** (see
[loop-and-running-state-design.md](loop-and-running-state-design.md)); `value`/`select`
supply typed inputs the graph cannot itself compute (see
[value-and-select-design.md](value-and-select-design.md)).

## 7. Execution engine

The engine runs a `pipeline.json` end to end with a **pipelined executor**: stage
parallelism, graph-derived arena sizing, per-node parallelism, and multi-input joins.
Durability is provided by **epoch-barrier checkpoint/restore** (resume after crash), and
overload is handled by explicit **backpressure** (stall or drop). Cross-process
observability surfaces worker restart counts and RPC latency.

## 8. Module lifecycle

Lifecycle is part of the node contract, modelled against Kubernetes probes / systemd
`sd_notify` / Triton. Defaults: `source`, `plc/control`, and `ai-model` are **resident**
(preloaded); short helpers are on-demand; heavy external workers are resident by default.
A module signals **readiness** after warmup so a slow start is a startup concern, not a
liveness failure. See [module-lifecycle-design.md](module-lifecycle-design.md).

## 9. Simulator-first

The platform is testable without vendor hardware. Simulator sources (folder sequence, loop
image, multi-frame, scenario) ship in-box, and the demo packages under `packages/` run the
full engine — including polyglot workers — with **no camera attached**.

## 10. AI inference

Model execution targets **ONNX Runtime**, hosted as a resident, preloaded node so cold-start
cost is paid once at startup rather than per frame.

## 11. Streaming & telemetry

- Optional pipeline signal streaming for observation (first choice: **WebSocket**).
- Media bridging via **MediaMTX** first, **GStreamer** only if later required.
- Telemetry is optional, non-blocking, best-effort; the hot path never waits on it.

## 12. Packaging

Pipelines are distributed as a **JSON + folder package**, not a single file, so models,
scripts, helper executables and configs travel together. Bindings live outside the package
so the same `pipeline.json` deploys everywhere.

## 13. Repository structure

```
src/core/        typed graph model + contracts (Mvf.Graph, Mvf.Abstractions)
src/engine/      pipelined scheduler, checkpoint/restore, backpressure
src/hosting/     out-of-process module host
src/transports/  shared-memory data plane
src/sdk/         dotnet / python / cpp SDKs
src/cli/         headless host + live TUI dashboard
modules/         integration modules (.NET + Python) + example module
packages/        runnable pipeline packages (pipeline.json)
protocol/        language-agnostic module wire protocol
tools/ · tests/ · docs/
```

The **core** knows only what a pipeline is (typed graph) and what a node contract is;
transports, module hosts and language SDKs attach at the edges so the core stays small.

## Technology defaults

Engine: **C# / .NET 10 LTS** · Studio UI (future): **React + TypeScript + React Flow** ·
optional desktop shell: **Tauri** · inference: **ONNX Runtime** · media: **MediaMTX** ·
telemetry: **WebSocket**.

## Related design docs

[roadmap.md](roadmap.md) ·
[pipeline-graph-foundation.md](pipeline-graph-foundation.md) ·
[data-plane-design.md](data-plane-design.md) ·
[module-lifecycle-design.md](module-lifecycle-design.md) ·
[value-and-select-design.md](value-and-select-design.md) ·
[loop-and-running-state-design.md](loop-and-running-state-design.md) ·
[integration-sdk-strategy.md](integration-sdk-strategy.md) ·
[sdk-quickstart.md](sdk-quickstart.md)
