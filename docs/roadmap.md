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
| M2.5 | **Snapshot + module-state recovery** (checkpoint/restore module state, worker-crash supervision, resume-after-crash) | 🟢 **C.1 + C.2 done.** C.1: module state captured/restored via a shared-memory slot (**no base64**); `SupervisedWorker` restarts a crashed child, restores it, and retries; the executor checkpoints stateful workers at cycle boundaries (`--checkpoint-every N`). C.2: captured states persist to a checkpoint dir (`--resume-dir`, atomic per-node files) and are **restored before the first cycle** on the next start, so a restarted process resumes; a clean, fully-consumed run clears them. Because a **source's position is just its checkpointable state**, the source resumes too — no separate cursor. Real-python worker crash recovers mid-run; a fresh executor resumes source + module state coherently (frames 3,4 not 1,2). Follow-up: a checkpointable **folder-sequence source** for a hardware-free resumable demo (Simulator-First) |
| M3 | Hardening: backpressure, crash recovery, warm pools, cross-process observability | 🟡 **Slice D.1 (backpressure) done.** Data-plane publish is now policy-driven: `BackpressurePolicy` (**Stall** = lossless, default; **Drop** = lossy) on `PipelineExecutionOptions`; the executor classifies a failed publish as transient (arena full → policy) vs permanent (payload larger than a slot → hard stop under any policy); `report.DroppedFrames` counts drops; CLI `execute-graph --backpressure stall\|drop`. Replaces the old silent "worker threw" fallback. Serial-executor caveat: Stall fails fast (no concurrent drain to wait on) — a real block-the-producer Stall + per-source policy override arrive with the pipelined executor. See `data-plane-design.md` §Backpressure. **Finding (2026-07-23): crash-recovery slot-reclaim is a non-gap** — the free-list is engine-owned and workers only read handed-in handles, so a dead worker holds no leases of its own; every engine allocation already has try/finally cleanup + `SupervisedWorker` retry. Remaining M3: **module lifecycle (L-track, below)**, cross-process observability |
| L | **Module lifecycle** — standards-aligned readiness contract (model/package/device/init all = "is this module ready?"). See `module-lifecycle-design.md` | 🟡 **Design agreed (standards-aligned)**; L.1 next. Absorbs the old M3 "warm pools" bullet |
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

### L-track — Module lifecycle (standards-aligned)
The trigger was "model lifecycle," but the real abstraction is **module readiness**: loading an ML model,
loading a package, connecting a camera/PLC, or finishing init are the same question — *"is this module
ready to do work, and how does the engine treat it while it is not?"* Today the contract is a lie
(`NodeActivationMode`/`activationMode` exist but nothing consumes them; everything is de-facto resident).
The L-track makes it **declared, observed, and enforced**, aligned to Kubernetes probes (startup/readiness/
liveness), OSGi bundle states, systemd `sd_notify`, and Triton model-control modes. Full design +
citations in [`module-lifecycle-design.md`](module-lifecycle-design.md).

| ID | Work item | Standard it mirrors | Status |
|----|-----------|---------------------|--------|
| **L.1** | Lifecycle contract **real & observed**: parse+validate `activationMode`→`NodeActivationMode` (reject unknown); `module.json` declares a default `lifecycle` profile; node overrides module default; executor **measures + reports per-node warmup (activation) duration** (`WarmupMs`). No behavior change for resident — the honest, low-risk first cut. | K8s startup probe; OSGi declared state | ⬜ Next |
| **L.2** | **Explicit readiness signal** over stdio: a worker emits `ready` when warmup completes; the engine waits for readiness (bounded by a startup budget) before routing; startup-vs-liveness separation so a slow model load isn't mistaken for a hang. | systemd `sd_notify READY=1`; K8s readiness/liveness split | ⬜ |
| **L.3** | **On-demand & idle-unload**: honor `OnDemand` — lazy-activate on first use, optional unload after N idle cycles. Needs a real short-helper node. | Triton EXPLICIT / lazy-load | ⬜ |
| **L.4** | **Warm pools**: pre-warmed worker instances so restart/scale hides cold-start. (Absorbs the old M3 "warm pools" bullet.) | Warm pools / K8s | ⬜ |
| **L.5** | **Hot-reload / package watch**: reload a module when its package changes. Frontier. | Triton POLL | ⬜ |

**Not in the L-track:** a real **ONNX inference node** is a separate, larger item (needs a model + hardware
story). The L-track defines the lifecycle *contract* that such a node — and a package loader, and a device
connector — all plug into. Contract first; heavy nodes inherit it.

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
