# MachineVisionFabric — Roadmap & Architecture Decisions

> Canonical, living plan. Supersedes `dataset-first-mvp-roadmap.md` (legacy direction).
> Product thesis: an open-source, .NET-native, **strictly-typed graph pipeline engine**
> whose value is **easy, rule-governed integration of heterogeneous nodes** ("write a
> node once, reuse it everywhere") — headless-first, with polyglot modules and a
> zero-copy data plane. It is NOT a machine-vision product; vision/PLC are just nodes.

## Guiding principle: minimal core, power at the edges
The **core** knows only *what a pipeline is* (typed graph) and *what a node contract is*.
It does **not** know how bytes move or what language/process a node runs in. Shared
memory, Python, gRPC, WASM are all **adapters behind core interfaces**. New capabilities
attach at the edges; the core stays small and stable.

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

---

## Milestones (work items + status)

| ID | Work item | Status |
|----|-----------|--------|
| M0 | Draw seams + folder/architecture; design docs | 🟡 In progress (this doc = first deliverable) |
| M1 | Out-of-process module host, Python-first — NO shared memory yet (frames copied over wire) | ⬜ Not started |
| M2 | Shared-memory zero-copy data plane, graph-aware | ⛔ Blocked — **discuss design with user first** |
| M3 | Hardening: backpressure, crash recovery, warm pools, cross-process observability | ⬜ Not started |
| M4 | Later frontiers: WASM tier, GPU handles (DLPack/CUDA-IPC), distributed | ⬜ Not started |

### M0 — Seams + structure (low risk, behavior unchanged)
- [x] Roadmap + architecture + captured design knowledge (this doc)
- [ ] Folder/project regroup toward the north-star structure *(pending Decision 1)*
- [ ] `protocol/` skeleton (message + frame-descriptor schema stub)
- Note: `ITransport` / `IModuleHost` seams are extracted **when their shape is known**
  (M1/M2), to avoid premature abstraction — not built speculatively in M0.

### M1 — Polyglot module host, Python-first (no data plane yet)
- Out-of-process worker host; module runs as a separate process, talks over the control
  plane. Frames are **copied over the wire for now** (intentionally slow but correct).
- Deliverable: a Python classifier node running inside the cognex pipeline.
- Proves protocol + Python SDK + lifecycle **independently of the hard data-plane work**.
- *(Decision 2 needed: gRPC vs stdio+protobuf/JSON for the control plane.)*

### M2 — Shared-memory data plane ⛔ GATE
**Do NOT implement before an explicit design discussion with the user.** The user will
decide, together with the assistant, how the pool / handle / lifetime model works. The
current design knowledge (below) is kept "in the pocket" and must be re-surfaced when we
reach this milestone.

### M3 / M4
Hardening then optional frontiers (see table).

---

## ⛔ Data-plane gate (read before starting M2)
The data plane will be **hand-written and custom** (not adopting iceoryx/Zenoh wholesale),
because the differentiator is that our engine **knows the static typed graph** and can do
things general middlewares cannot (precomputed refcounts/routing). Before writing any of
it: **stop and design it with the user.** Reminder is also stored in memory.

## Appendix — captured design knowledge (keep in pocket for M2)
- **Control plane vs data plane split (physical).** Small messages (orchestration,
  handles, results, backpressure) cross the language boundary over RPC/stdio. Big frames
  never cross as bytes — only a **handle** does. This maps onto our data-edge/control-edge
  distinction.
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
  (shared-mem handle) | (later) cross-machine (must serialize) . Scheduler picks the cheapest.
- **GPU frontier (M4):** keep frames GPU-resident; pass CUDA-IPC / **DLPack** handles so a
  Python torch tensor wraps the same buffer with no copy. Real bottleneck is host↔device
  copy, not IPC.
- **Build vs adopt:** ADOPT the control plane (gRPC/stdio — mature). BUILD the data plane,
  but **minimal and graph-aware** — a narrow, single-machine, topology-specific slot pool,
  not a general IPC middleware. Kept as a separate transport project the core never depends on.

## Open decisions (pending user)
- **Decision 1 (M0):** folder regroup — low-churn (keep names, add new folders for new
  projects) **[assistant default]** vs full clean rename now (Contracts+Core → Mvf.Graph +
  Mvf.Abstractions, real-world-projects/integrations → modules/).
- **Decision 2 (M1):** control plane — **gRPC** vs **stdio + protobuf/JSON**.
