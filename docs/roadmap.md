# MachineVisionFabric — Roadmap & Architecture Decisions

> Canonical, living plan.
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
modules/       cognex, dark-frame-filter, black-screen-check, dataset-writer, dotnet-brightness-gate (example)
packages/      inspection-demo, loop-demo, value-demo, multilang-demo, py-brightness-demo, py-invert-demo
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
Wired into CLI `validate-pipeline` + `execute-graph`; `packages/inspection-demo/pipeline.json`
is lean. Tests 39/39.

---

## Milestones (work items + status)

| ID | Work item | Status |
|----|-----------|--------|
| M0 | Draw seams + folder/architecture; design docs | 🟡 In progress (docs + north-star folder/project rename done; ITransport/IModuleHost seams deferred to M1/M2) |
| M1 | Out-of-process module host (Python), control plane over local **stdio** | 🟢 Slices 1–2 done — Python classifier auto-wired from its manifest `runtime`; `IOutOfProcessModuleHost` seam + `StdioModuleHost`; real-python e2e test green |
| M2 | Our own **shared-memory** zero-copy data plane, graph-aware (no network) — see `data-plane-design.md` | 🟢 Slices A + B + **B.3 done**. Engine-owned arena behind `IDataPlane`; graph-aware publish-once fan-out; generic **typed-payload** plane (DLPack-style `PayloadDescriptor` in the slot header, engine-validated; SDKs wrap bytes as zero-copy `memoryview`/numpy / `Span`; **no base64**). **Worker transformer** (frame-out): the engine pre-allocates the output slot, the child writes a new frame into the arena, and **live-edge-occupancy refcounts** (AddRef on re-emit before releasing consumed inputs) keep pass-through/fan-out balanced. Real-python classifier + transformer round-trips green. Next: **snapshot + module-state recovery** (M2.5) |
| M2.5 | **Snapshot + module-state recovery** (checkpoint/restore module state, worker-crash supervision, resume-after-crash) | 🟢 **C.1 + C.2 done.** C.1: module state captured/restored via a shared-memory slot (**no base64**); `SupervisedWorker` restarts a crashed child, restores it, and retries; the executor checkpoints stateful workers at cycle boundaries (`--checkpoint-every N`). C.2: captured states persist to a checkpoint dir (`--resume-dir`, atomic per-node files) and are **restored before the first cycle** on the next start, so a restarted process resumes; a clean, fully-consumed run clears them. Because a **source's position is just its checkpointable state**, the source resumes too — no separate cursor. Real-python worker crash recovers mid-run; a fresh executor resumes source + module state coherently (frames 3,4 not 1,2). Follow-up: a checkpointable **folder-sequence source** for a hardware-free resumable demo (Simulator-First) |
| M3 | Hardening: backpressure, crash recovery, warm pools, cross-process observability | 🟡 **Slice D.1 (backpressure) done.** Data-plane publish is now policy-driven: `BackpressurePolicy` (**Stall** = lossless, default; **Drop** = lossy) on `PipelineExecutionOptions`; the executor classifies a failed publish as transient (arena full → policy) vs permanent (payload larger than a slot → hard stop under any policy); `report.DroppedFrames` counts drops; CLI `execute-graph --backpressure stall\|drop`. Replaces the old silent "worker threw" fallback. **Per-source override done:** a producing node's `backpressure` field (or a module-declared default) overrides the run default, resolved node→module→run-default; validator rejects `pipeline.node.invalid-backpressure`. Serial-executor caveat: Stall fails fast (no concurrent drain to wait on) — a real block-the-producer Stall arrives with the pipelined executor. See `data-plane-design.md` §Backpressure. **Finding (2026-07-23): crash-recovery slot-reclaim is a non-gap** — the free-list is engine-owned and workers only read handed-in handles, so a dead worker holds no leases of its own; every engine allocation already has try/finally cleanup + `SupervisedWorker` retry. **Slice E (cross-process observability) done (2026-07-23):** a supervised restart is transparent by design, so the run used to end with no record that a child ever died. `SupervisedWorker` now counts restarts (cold vs warm-spare, last time + reason); the worker adapters time every `execute` RPC from the engine side (count, failures, avg/max — microsecond resolution, so a restart shows as a latency spike). Both travel through the `IWorkerMetricsSource` seam (mirrors `ICheckpointable`: adapter → node runner → executor, core never sees stdio) into `NodeExecutionStats.Worker` + `report.WorkerRestarts`, harvested before runners are disposed. CLI prints `restarts:N` in the headline and a per-node `worker=… rpc=… restarts=… avg=… max=…` line (both `--no-tui` and, newly, after the TUI run); the TUI logs a warning the cycle a restart is absorbed, badges the node `r{n}`, and shows `rst:` in the header. Also fixed: the TUI path was dropping `--checkpoint-every`/`--resume-dir`/`--backpressure` (options weren't copied into the dashboard's run). Suite 93/93. **Slice F (source-failure honesty) done (2026-07-23):** found while verifying E — a camera that never connected finished as a clean, *successful* run of zero cycles, and cleared its resume checkpoint on the way out. Three layers were each losing the signal: the Cognex session caught its producer exception and returned normally (so the SDK completed the frame channel cleanly); `BackgroundFrameSourceSession.DisposeAsync` would have rethrown a producer fault as a second, duplicate failure; and the executor treated a *faulted* source exactly like an exhausted one (`faulted → NoOutput → sourcesExhausted → Succeeded=true`). Now: the module rethrows after logging, dispose swallows (the channel is the delivery path), and a faulted source ends the run with `Succeeded=false` + `Source node 'x' failed: …` while **keeping** its checkpoint — a failed source has not consumed its stream. A genuinely exhausted source still succeeds and still clears. Suite 96/96. Remaining M3: **module lifecycle (L-track, below)** — L.1–L.4 done, L.5 + L.3b design-gated |
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
| **L.1** | Lifecycle contract **real & observed**: parse+validate `activationMode`→`NodeActivationMode` (reject unknown); `module.json` declares a default `lifecycle` profile; node overrides module default; executor **measures + reports per-node warmup (activation) duration** (`WarmupMs`). No behavior change for resident — the honest, low-risk first cut. | K8s startup probe; OSGi declared state | 🟢 **Done.** `NodeActivationModes.TryParse`; `PipelineNodeDefinition.ActivationMode` now nullable (null = inherit); `ModuleManifest.Lifecycle`; validator rejects `pipeline.node.invalid-activation-mode`; executor resolves node→module→resident + times activation into `NodeExecutionStats.WarmupMs`/`ActivationMode`; CLI prints `mode=/warmup=`. Cognex manifest declares `lifecycle: resident`. Suite 81/81 |
| **L.2** | **Explicit readiness signal** over stdio: a worker emits `ready` when warmup completes; the engine waits for readiness (bounded by a startup budget) before routing; startup-vs-liveness separation so a slow model load isn't mistaken for a hang. | systemd `sd_notify READY=1`; K8s readiness/liveness split | 🟢 **Done.** SDK `on_start` warmup hook → `hello ready:false` + `ready` signal; `StdioWorkerProcess.StartAsync` waits for `ready` bounded by `WorkerLaunchInfo.StartupBudget` (default 30s); a budget overrun is a distinct `WorkerStartupException` (startup, not liveness). `WorkerLaunchInfo.Environment` for child env. Demo module `py-warmup-classifier`; protocol doc updated. Real-python tests: warmup→ready→serve, and budget-overrun→WorkerStartupException. Suite 83/83 |
| **L.3** | **On-demand & idle-unload**: honor `OnDemand` — lazy-activate on first use, optional unload after N idle cycles. Needs a real short-helper node. | Triton EXPLICIT / lazy-load | 🟢 **Core done** (lazy-activate + idle-skip). Executor resolves modes first, preloads resident nodes, and activates an on-demand node only the cycle a frame first reaches it (timing its warmup), skipping it while idle — a gated helper costs nothing until used. Restore-on-start folded into per-node activation so a lazily-activated node still resumes. Lazy-activation failure degrades gracefully (warn + skip). Suite 85/85. **Deferred → L.3b: idle-unload** (dispose after N idle cycles + re-activate) — riskier (state/re-warmup), opt-in, once a stateful on-demand user exists |
| **L.4** | **Warm pools**: pre-warmed worker instances so restart/scale hides cold-start. (Absorbs the old M3 "warm pools" bullet.) | Warm pools / K8s | 🟢 **Done.** `WarmWorkerPool` pre-warms N spare workers (spawn + L.2 warmup); `AcquireAsync` hands out a ready spare instantly and replenishes in the background, cold-spawn fallback when empty, dead-spare discard. `SupervisedWorker` takes an optional pool → a crash restart swaps in a pre-warmed spare (no cold-start on the recovery hot path) then restores state; owns+disposes the pool. `StdioModuleHost` enables it per worker via `MVF_WARM_SPARES` (default 0 = cold restart, unchanged). Real-python tests: pool pre-warms to target + acquire returns a ready worker; supervised recovery via a warm spare with state intact. Suite 87/87 |
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
- **Pipelined executor (2026-07-23):** ✅ Gate opened, shape agreed; not started. The serial executor
  stays as-is and pipelined arrives as an **opt-in mode behind `IPipelineGraphExecutor`**, so the
  deterministic path (and the suite) is untouched. Agreed:
  1. **Checkpointing = epoch barrier** — drain the pipeline every N frames, then snapshot. Keeps M2.5's
     "quiesced ⇒ torn-free" guarantee literally true instead of replacing it; aligned/Chandy-Lamport
     barriers are a later option if the drain hiccup ever costs real throughput.
  2. **Strict source order at sinks, always.** Not a per-port opt-out — for inspection and traceability
     an out-of-order result is worse than a slower one.
  3. **Per-node parallelism (N worker instances) is in phase 1**, not deferred.
  (3) is what makes this more than a refactor, and it collides with recovery: **a stateful node cannot be
  replicated** — N instances means N divergent states, and M2.5 keeps exactly one `<nodeId>.state` per node.
  So parallelism must be *declared and validated*: `parallelism > 1` is legal only for a module that is
  stateless (no `on_checkpoint`/`on_restore`), and the validator rejects the combination — same shape as
  the existing `activationMode`/`backpressure` resolution. It also needs frame **sequence numbers as a
  first-class ordering key**, a **bounded reorder buffer** on a parallel stage's output (a third
  backpressure surface, and the one that can head-of-line block), and `WarmWorkerPool` generalized from
  restart-spares to a live instance pool. Arena sizing stops being a constant: in-flight slots =
  Σ(queue depths) + Σ(instances), so slot count becomes computable from the graph.
- **Pipelined executor — step 1 built (2026-07-23).** Opt-in `--mode pipelined [--queue n]`; serial stays
  the default and unchanged. **Measured 1.91× on the 6 MB numpy bench** (210 → 402 f/s); the gain tracks how
  much work the worker actually does, so a no-op worker gains nothing. `NodeExecutionStats.Stage`
  (busy/route/writeBlocked/readBlocked) makes that legible and finally attributes the arena publish, which
  no node owned before. **Step 1b: multi-input joins**, via a **void marker** — every edge carries exactly
  one message per cycle, so a join reads one from each edge and pairs by construction, and an untaken
  switch branch cannot stall it. `multilang-demo` now runs pipelined with byte-identical routing to serial.
  **Step 1c: epoch-barrier checkpointing.** Every N cycles the source stops feeding and waits for the
  pipeline to drain, then captures, then resumes — so M2.5's "quiesced ⇒ torn-free" contract stays literally
  true instead of being redefined. Drained is decided **at the leaves**: every node reaches some leaf, edges
  are FIFO with one message per cycle, so once every leaf has finished cycle C (and released its arena
  inputs) nothing upstream is in flight. Restore-on-start and clear-on-clean-completion match serial;
  `CheckpointCoordinator` is now shared by both executors. **Measured barrier cost** on the 6 MB numpy
  bench: `--checkpoint-every 100` is indistinguishable from no checkpointing (5.18 s vs 5.19 s),
  `--checkpoint-every 10` costs ~10% (5.69 s) — roughly one pipeline depth of latency per barrier,
  amortised over N.
  **Step 1d: per-node parallelism.** `"parallelism": N` on a node runs N instances of it; results pass
  through a single emitter that restores source order, so the always-strict-order decision holds without
  the caller doing anything. Replication is **opt-in at the module** (`maxParallelism` in `module.json`,
  default 1) because only the author knows whether it keeps state across frames — the engine cannot see
  that, and N instances of a stateful module means N silently diverging states; a node asking for more
  than its module allows fails with both numbers rather than being clamped. Measured (6 MB, 2000 cycles):
  serial 8.63 s → pipelined 5.25 s → 2 instances **4.10 s**, and the stage profile shows the bottleneck
  moving off the worker (`readBlocked` 3.9 s) onto the arena publish (`cam route` 3.9 s) — the ceiling of
  *this* graph, where publish and compute are comparable, not of the mechanism.
  Still refused, with the reason: two edges into one input port.
  **Step 1e: graph-derived arena sizing — phase 1 complete.** Slot count now comes from the graph
  (`queue + 3 × instances` per out-of-process node, plus one for the frame a producer holds), computed
  after expansion and fed to the arena before anything resolves it; `--arena-slots` overrides. Sized
  rather than policed on purpose — a conservative pre-flight check would have rejected pipelines that run
  fine today, since the worst case is rarely reached at once. Serial and single-instance pipelined still
  land on the historical 8, so nothing regresses. This unblocked `parallelism: 4`: 6 MB/2000 cycles reads
  serial 8.63 s → pipelined 5.25 s → ×2 4.10 s → **×4 3.65 s (2.36× over serial)**, flattening from ×2 to
  ×4 because `cam.route` is then the wall clock. Cost is honest: 17 slots × 8 MB ≈ 136 MB.
- **`value` + `select` primitives — built (2026-07-23).** Two values a dataflow graph could not express:
  one the graph cannot compute (a threshold, an output folder, which camera), and narrowing a collection.
  Kept as **separate primitives** — producing a value has nothing to do with collections, and merging them
  would give one node with two unrelated jobs and a config where half the fields are always ignored. The
  interactive camera picker is then not a feature but `discover → select` with an unresolved criterion,
  which is one composition among many.
  Primitives rather than a module, because the engine needs a binding store, resolution before the cycle
  loop and validator typing rules either way; a module would add indirection on top of that *and* make the
  graph language depend on a plugin being installed. What stays outside the core is *how* a value is
  obtained (`IValueResolver`) and *discovering* candidates (a module) — the same minimal-core shape as
  `IDataPlane` and `IOutOfProcessModuleHost`.
  Types are ordinary: `control/value:<t>` and `control/list:<t>` over `string|int|number|bool|json`,
  checked by the existing edge rule. A `json` value may declare a JSON Schema, enforced wherever a value
  enters the graph — which keeps "the camera record" typed without the core knowing what a camera is.
  **Resolution happens before cycle 0**, in a CLI pre-pass: the loop runs per frame, so prompting inside
  it is absurd and blocking on a human breaks unattended operation (the TUI repaints every 120 ms and
  could not share the terminal anyway). Bindings live in `.mvf/bindings.json`, **outside the package**, so
  the same `pipeline.json` deploys to ten panel PCs byte-identically and each binds to its own camera.
  Unattended is the default posture: `--no-prompt`, an environment variable per binding, and an
  unresolved value fails before cycle 0 naming the binding and the file to set it in. Serial only for now
  — pipelined refuses both primitives up front, since a node with no inputs has no queue to pace it.
  Design and open questions in `docs/value-and-select-design.md`.
- **Live tuning (2026-07-23).** A resolved `value` is a constant but not a frozen one: each one registers
  a `LiveValue`, and the TUI edits it mid-run (tab/↑↓ to pick, enter to edit); the node picks the change up
  on its **next** cycle. This does not undo moving prompts out of the loop — that rule was never "values
  must not change", it was **the loop must never wait for a human**. A prompt binds the pipeline to a
  person; an asynchronous setting change does not. Cost on the hot path is one volatile read, and the
  emitted result is rebuilt only when the setting actually changed. A new setting goes through the same
  type + schema check as a literal or a stored binding — a running graph is the last place that should
  accept an ill-typed value. **What is tunable is decided by when the value is consumed:** a threshold is
  consumed per frame, so turning it means something; which camera to open is consumed at activation, so
  changing it is reconfiguration, not tuning, and is not offered. Changes persist to the binding
  immediately; a value with no binding is tunable but not persisted, because its only durable home would
  be a literal in `pipeline.json` — the portable artifact a per-machine tuning session must not rewrite.
  No mouse: Spectre.Console's live display gives no mouse events, so click-to-edit belongs to the studio.
- **List-shaped values (2026-07-23).** `"shape": "list"` on a `value` makes its port `control/list:<t>`,
  which is what a `select`'s items port consumes — so `value → select ← value` is a real chain today,
  before the discovery module exists. Justified rather than convenient: `select` already emits a
  collection in `mode: many`, a set of candidates *is* one value the graph cannot compute, and a literal
  candidate list is the "simulator first" stand-in the project asks for everywhere else. For a list the
  declared schema describes an **element**, so the same schema reads the same whether a record arrives
  alone or in bulk. `packages/value-demo` now wires three values into two selects — each with its own
  type, binding and consumer, which is the load test for "`value` produces one value, it is not a form".
- **The picker is live (2026-07-23).** The pre-pass walks the edge into `select.items`; when the producer
  is a list-shaped `value` whose collection is already settled, it hands those elements to the resolver as
  `ValueRequest.Choices`, and the terminal renders a list to pick from instead of asking for an identifier
  the operator would have to read off another screen. With a `by`, what is stored is that property of the
  chosen element, not the whole record — so the binding survives a later discovery run that returns the
  same camera with different incidental fields. The pre-pass now walks in **topological order**, which is
  what guarantees a collection is settled before anything offers it. Still missing: candidates from a
  *module*, which means activating and running a discovery node before cycle 0 — a real decision about
  pre-pass lifecycle rather than a lookup. Everything downstream of the seam is done.
  This also made the two criterion sources readable as an authoring choice: a `select` with its own
  `binding` is picked once per machine and then silent; a criterion arriving on an edge from a `value` node
  is a live tunable instead. `packages/value-demo` now shows both.
- **Picking mid-run (2026-07-23).** A `select` whose criterion is its own binding is now a live tunable,
  and one that picks rather than types: the runner publishes the collection it is narrowing every cycle
  (reference-compared, so a steady list costs one comparison), and the dashboard renders those as a
  selection list. The candidates are therefore the ones the graph is narrowing *right now*, not the ones
  resolved at startup — which is what makes this work later when discovery re-runs. A choice stores the
  property named by `by`, the same rule the first-run picker follows, so both write the same binding.
  A `select` fed by an **edge** is deliberately not offered: the edge would overwrite the tuning on the
  next cycle, and a control that silently does nothing is worse than no control. The pre-pass marks those
  and the activator skips them; the `value` behind the edge is the tunable instead.
  `LiveValue.PublishChoices` is public where `Set` is internal, and the asymmetry is the contract — a new
  setting must pass the type/schema check, whereas candidates are an observation of what the graph already
  carries.
