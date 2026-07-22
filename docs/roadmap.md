# MachineVisionFabric — Roadmap & Architecture Decisions

> Canonical, living plan. Supersedes `dataset-first-mvp-roadmap.md` (legacy direction).
> Product thesis: an open-source, .NET-native, **strictly-typed graph pipeline engine**
> whose value is **easy, rule-governed integration of heterogeneous nodes** ("write a
> node once, reuse it everywhere") — headless-first, with polyglot modules and a
> zero-copy data plane. It is NOT a machine-vision product; vision/PLC are just nodes.

## Guiding principle: minimal core, power at the edges
The **core** knows only *what a pipeline is* (typed graph) and *what a node contract is*.
It does **not** know how bytes move or what language/process a node runs in. Shared
memory, Python, stdio, WASM are all **adapters behind core interfaces**. New capabilities
attach at the edges; the core stays small and stable.

## Hard constraint: NO network — everything is local IPC
All inter-process communication is **local to one machine**. There is no network transport
anywhere in the runtime: **no TCP, no gRPC/HTTP, no sockets-over-the-wire.**
- **Data plane** = our own **shared memory** (zero-copy handle passing). Hand-written.
- **Control plane** = **stdio pipes** (length-prefixed messages) between the engine and each
  co-located child process. Local, dependency-free.
Cross-machine / distributed operation is an explicit **non-goal**.

## Target architecture (layers)
```
1. Graph & Types       typed graph model + validation (pure data + rules, no execution)
2. Execution contracts  INodeRunner, PortValue, IFrameEnvelope, ITransport, IModuleHost
   ^^^ 1+2 = CORE. Small, stable, dependency-light. Knows nothing of shm/Python.
------------------------------------------------------------------------------------
3. Engine / scheduler   topological execution, cycles, backpressure. Moves PortValues;
                        does not know how bytes cross a boundary.
4. Transports (data)    ITransport impls: in-process (zero-copy) | shared-memory (M2)
5. Module hosts         IModuleHost impls: in-process .NET | out-of-process worker (M1)
6. Module SDKs          .NET SDK (exists) | Python SDK (M1)
7. Host apps            CLI incl. TUI (exists)
```

## Target folder structure (north star)
```
src/
  core/        Mvf.Graph (model+validation), Mvf.Abstractions (contracts+interfaces)
  engine/      Mvf.Engine (scheduler + node runners)
  transports/  Mvf.Transport.InProcess, Mvf.Transport.SharedMemory (M2)
  hosting/     Mvf.Hosting.InProcessDotnet, Mvf.Hosting.Worker (M1)
  sdk/         dotnet/Mvf.Sdk, python/mvf_sdk (M1)
  cli/         Mvf.Cli
protocol/      language-agnostic wire contract (.proto / schema): control msgs + frame descriptor
modules/       cognex, dark-frame-filter, black-screen-check, dataset-writer (from real-world-projects/integrations)
packages/      cognex-dark-capture
docs/
```
Most of this is a **move/regroup**, not new code. New: `protocol/`, `transports/SharedMemory`,
`hosting/Worker`, `sdk/python`.

## Baseline (already done — Faz 1–4)
- Typed graph engine: `execute-graph` runs a `pipeline.json` end to end.
- Node runners: source / processor / sink / gate / fork / if / switch / **classifier**.
- Legacy manifest+profile runner, Host and Storage projects removed (graph is the only path).
- Manifests use readable string `kind`; `inspect-session` reads the module's real session.json.
- First-class **frame→control classifier** (perception→control) + `ControlSignal.Measurement`.

## Authoring: lean pipeline format (done)
Authors write only what is not derivable; a `PipelineExpander` fills in the rich model
**before validation**, so the validator and executor stay unchanged.
- module node: `{ "id": "blackCheck1", "module": "mvf.black-screen-check", "config": {…} }`
- primitive node: `{ "id": "fork1", "primitive": "fork" }` (fork/switch outputs derived from the
  ports on its leaving edges; `if` outputs are fixed)
- edge: `{ "from": "camera1.frame", "to": "fork1.frame" }` (id auto; kind = source port channel)

Ports + category come from the module's `kind` via a metadata-only `ModuleCatalog` (reads
`module.json`, **no DLL load**): source→source, processor→compute, classifier→classify,
gate→control, sink→output. Rich nodes/edges still pass through unchanged (mixed files work).
Wired into CLI `validate-pipeline` + `execute-graph`; `packages/cognex-dark-capture/pipeline.json`
is now lean. Tests 39/39.

---

## Milestones (work items + status)

| ID | Work item | Status |
|----|-----------|--------|
| M0 | Draw seams + folder/architecture; design docs | 🟡 In progress (docs + north-star folder/project rename done; ITransport/IModuleHost seams deferred to M1/M2) |
| M1 | Out-of-process module host (Python), control plane over local **stdio** | 🟢 Slices 1–2 done — Python classifier auto-wired from its manifest `runtime`; `IOutOfProcessModuleHost` seam + `StdioModuleHost`; real-python e2e test green |
| M2 | Our own **shared-memory** zero-copy data plane, graph-aware (no network) — see `data-plane-design.md` | 🟢 Slices A + B + **B.3 done**. Engine-owned arena behind `IDataPlane`; graph-aware publish-once fan-out; generic **typed-payload** plane (DLPack-style `PayloadDescriptor` in the slot header, engine-validated; SDKs wrap bytes as zero-copy `memoryview`/numpy / `Span`; **no base64**). **Worker transformer** (frame-out): the engine pre-allocates the output slot, the child writes a new frame into the arena, and **live-edge-occupancy refcounts** (AddRef on re-emit before releasing consumed inputs) keep pass-through/fan-out balanced. Real-python classifier + transformer round-trips green. Next: **snapshot + module-state recovery** (M2.5) |
| M2.5 | **Snapshot + module-state recovery** (checkpoint/restore module state, worker-crash supervision, resume-after-crash) | 🟡 C.1 step 1 done — `ICheckpointable` + checkpoint/restore protocol; module state travels through a shared-memory slot (**no base64**); real-python round-trip restores a stateful module's state into a fresh worker. Next: worker-death supervision + auto-restart+restore (C.1 step 2), then engine-crash resume (C.2) |
| M3 | Hardening: backpressure, crash recovery, warm pools, cross-process observability | ⬜ Not started |
| M4 | Later frontiers: WASM tier, GPU handles (DLPack/CUDA-IPC) — **distributed is a non-goal** | ⬜ Not started |

### M0 — Seams + structure (low risk, behavior unchanged)
- [x] Roadmap + architecture + captured design knowledge (this doc)
- [x] North-star folder/project rename: `Contracts→Mvf.Graph`, `Core→Mvf.Abstractions`,
      `Runtime→Mvf.Engine`, `Sdk→Mvf.Sdk`, `Cli→Mvf.Cli`; `real-world-projects/*`→`modules/`
      + `packages/`; solution `Mvf.slnx`. Module (plugin) namespaces kept as
      `MachineVisionFabric.Integrations.*` to protect string-coupled manifests. Build clean,
      tests 29/29, no-flag CLI verified.
- [ ] `protocol/` skeleton (message + frame-descriptor schema stub) — start of M1
- Note: `ITransport` / `IModuleHost` seams are extracted **when their shape is known**
  (M1/M2), to avoid premature abstraction — not built speculatively in M0.

### M1 — Polyglot module host, Python-first
- Out-of-process worker host; a module runs as a separate co-located process and talks to
  the engine over **local stdio** (newline-delimited JSON; protobuf later). No network.
- For M1 the frame is carried **inline over the stdio pipe (base64 copy)** — a local pipe,
  not the network. M2 replaces the payload with a shared-memory handle (zero copy).
- **Slice 1 (done):** protocol (`protocol/README.md`), Python SDK (`src/sdk/python/mvf_sdk`),
  sample `modules/py-brightness-classifier`, and `Mvf.Hosting.Worker` — a
  `WorkerFrameClassifier : IFrameClassifier` that spawns the Python process and drops into the
  existing FrameClassifierNodeRunner unchanged. End-to-end test spawns python3 and asserts
  black/ok classification. Targets: Python + Node.js + out-of-proc .NET (same JSON protocol).
- **Slice 2 (done):** a `runtime: "python"` classify node is auto-wired. The activator reads the
  module's `runtime`/`entry` from the metadata-only `ModuleCatalog` (no DLL load); for a non-`dotnet`
  runtime it goes through the `IOutOfProcessModuleHost` seam (impl `Mvf.Hosting.Worker.StdioModuleHost`),
  which spawns the worker and returns a `WorkerFrameClassifier` that drops into the existing
  `FrameClassifierNodeRunner`. The core scheduler/activator never reference stdio/Python — hosting is
  an adapter behind the seam. `FrameClassifierNodeRunner` now disposes a worker-backed classifier so the
  child process is shut down. Demo: `packages/py-brightness-demo/pipeline.json` (lean) routes Python
  `black`/`ok` labels through a `switch`; a real-python e2e test asserts classification via the activator.
  Next capabilities (processor/sink over a worker) reuse the same seam.
- **Decision 2:** ✅ Resolved — control plane = **stdio + JSON** (protobuf later), no network/gRPC.

### M2 — Shared-memory data plane ⛔ GATE (cleared for Slice A)
**Gate:** don't implement before an explicit design discussion with the user. **That discussion
happened (2026-07-22) and the user approved the impl decisions** (file-backed MMF arena; segregated
free-list; separate transport project the core never references) — see the "Implementation decisions"
section of `data-plane-design.md`. **Slice A is built** (arena + Python frame path over shm handles).
Re-open the gate before **Slice B** (module-requested allocation, graph refcounts, the `IDataPlane`
seam, context slot) and **Slice C = M2.5** (module state slot + snapshot + resume) — those introduce
the harder lifetime/ownership choices and should be talked through as they come up.

### M3 / M4
Hardening then optional frontiers (see table).

---

## ⛔ Data-plane gate (read before starting M2)
Confirmed by the user: the data plane is **hand-written, our own, over shared memory, with
NO network** (not adopting iceoryx/Zenoh) — because the differentiator is that our engine
**knows the static typed graph** and can do things general middlewares cannot (precomputed
refcounts/routing). The transport decision is settled; the **detailed design** (pool/handle/
lifetime/backpressure/signaling) is still to be worked out **together with the user**. Before
writing implementation: **stop and design it with the user.** Reminder is also stored in memory.

**The design decisions are now agreed and captured in [`data-plane-design.md`](data-plane-design.md)**
(ownership/mutation, variable auto-sized buffers, precomputed refcounts, engine-owned pool,
descriptor-in-header, engine context slot, engine-allocated module state, snapshot recovery).

## Appendix — captured design knowledge (keep in pocket for M2)
- **Control plane vs data plane split (physical).** Small messages (orchestration,
  handles, results, backpressure) cross the language boundary over **local stdio** (no
  network). Big frames never cross as bytes — only a **handle** into shared memory does.
  This maps onto our data-edge/control-edge distinction.
- **Modules are co-located (same machine).** So shared memory is the sweet spot; no network.
- **Shared-memory slot pool.** Engine opens ONE shared segment carved into fixed-size
  **slots**; every process maps the same segment. A frame is written into slot N once;
  downstream nodes read it **in place** (zero copy). Passing a frame = passing slot N +
  a typed descriptor (w/h/stride/format/seq/timestamp). Borrow/return per frame from a
  shared free-list. NOT per-module private regions (that would force copies).
- **Graph-aware optimization (the reason to build our own).** Because the graph is static
  and typed, fan-out (hence initial **refcount**) and routing are known at build time and
  precomputed; slot sizing can be tiered by edge datatype; "frame no longer needed here"
  points come from static analysis. General middlewares (iceoryx/Zenoh) don't know topology.
- **Hard problems to design:** slot lifetime/ownership (refcount vs move vs copy-on-write —
  matters if a node *mutates* the frame), free-list, readiness signaling (event/semaphore
  vs embedded in control RPC; spin-then-block), backpressure (credit-based when pool full),
  crash cleanup (supervisor reclaims slots of a dead module), descriptor location.
- **Transport tiers:** in-process .NET↔.NET (identity, zero copy) | co-located out-of-proc
  (shared-mem handle). Cross-machine is a non-goal. Scheduler picks the cheapest.
- **GPU frontier (M4):** keep frames GPU-resident; pass CUDA-IPC / **DLPack** handles so a
  Python torch tensor wraps the same buffer with no copy. Real bottleneck is host↔device
  copy, not IPC. (Still local — no network.)
- **Build vs adopt:** Control plane = **local stdio pipes** (no network, no gRPC/TCP — plain
  child-process stdin/stdout). BUILD the data plane over **shared memory**, minimal and
  graph-aware — a narrow, single-machine, topology-specific slot pool, not a general IPC
  middleware. Kept as a separate transport project the core never depends on.

## Open decisions
- **Decision 1 (M0):** ✅ Resolved — full clean rename to the north-star structure (done).
- **Decision 2 (M1):** ✅ Resolved — control plane = **stdio + protobuf/JSON**, local only,
  no network/gRPC.
- **Data plane (M2):** ✅ Transport decided — our own **shared memory**, no network. Detailed
  design still to be done together with the user (gated).
