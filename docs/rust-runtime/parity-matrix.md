# Parity matrix — .NET reference vs Rust runtime

The .NET engine (`src/`, 257 xUnit tests in `tests/Mvf.Engine.Tests`, 48 files) is the behavioural
reference. This matrix records, per behaviour, where the Rust runtime stands. Status values:

- **ported** — implemented in Rust with tests that assert the same behaviour;
- **partial** — a documented subset is implemented;
- **new** — Rust-only behaviour from #10 with no .NET equivalent;
- **not yet** — not implemented; nothing in Rust claims it.

Updated by every slice; the stage reports carry the details.

## Contract and schema

| Behaviour (.NET reference) | Reference | Rust | Status |
|---|---|---|---|
| Module manifest `id/name/version/kind/runtime/entry` | `ModuleManifest.cs` (`module.json`) | `NodeContract.metadata` + `spec.artifacts` | partial — `kind` (port derivation) is replaced by explicit typed ports; `name` → `description` |
| Ports derived from capability kind (source/processor/classifier/analyzer/gate/sink) | `IntegrationCapabilityKind` | explicit `spec.ports` per node | new — explicit typing replaces derivation |
| `runtime: dotnet \| python \| node` | `ModuleManifest.Runtime` | `artifacts[].backend` + `worker.runtime: python \| executable` | partial — Python ported (existing SDK modules run unchanged); `node` not mapped; .NET modules → OCI (#18) |
| `"module": "py.…"` references | pipeline JSON | `module:` + `modules/<dir>/node.yaml` | ported |
| `lifecycle: resident \| on-demand` | `ModuleManifest.Lifecycle`, `OnDemandActivationTests` | `spec.lifecycle.activation` | ported (#25) — lazy start, state handed over at activation; activation failure goes through `onError` |
| `backpressure: stall \| drop` | `ModuleManifest.Backpressure` | node `backpressure`, `run.backpressure` | ported (#24) |
| `maxParallelism` | `ModuleManifest.MaxParallelism` | `spec.execution.maxParallelism` + node `parallelism` | ported (#25) — refused for `stateful` contracts |
| Readiness budget (`hello ready:false` → `ready`) | `protocol/README.md`, `WorkerReadinessTests` | `spec.lifecycle.readinessTimeoutMs` | ported |
| Typed payload descriptor (192-byte header, `MVF1`) | `PayloadDescriptor.cs` | `mvf_memory::Descriptor` | ported — byte-for-byte golden + Python SDK cross-check |
| Isolation / resource / memory / placement policy | — | `spec.isolation`, `spec.resources`, `spec.memory`, `spec.execution` | new |
| Live replacement + versioned state | — | `spec.lifecycle.replacement`, `spec.lifecycle.state`, `mvf_engine::replace` | new — stateless + stateful (in-process, worker since #24), rollback, update plans |
| Backend capability negotiation | — | `mvf_contract::negotiate` | new |
| Strict YAML (dup keys, no anchors/tags, bounded) | JSON via `System.Text.Json` | `mvf_contract::yaml` | new |

## Graph validation (`PipelineDefinitionValidator`, 31 codes)

| .NET code | Rust | Status |
|---|---|---|
| `pipeline.node.duplicate-id`, `missing-id` | `graph.node.duplicate-id`, `graph.node.invalid-id` | ported |
| `pipeline.edge.missing-source-node/port`, `missing-target-node/port` | same names under `graph.edge.*` | ported |
| `pipeline.edge.data-type-mismatch` | `graph.edge.type-mismatch` | ported |
| `pipeline.edge.source-channel-mismatch`, `target-channel-mismatch` | `graph.edge.channel-mismatch` (edge lists are per channel) | ported |
| — | `graph.cycle`, `graph.input.multiple-edges`, `graph.input.unconnected-required`, `graph.edge.duplicate`, `graph.node.no-source` | new (stricter) |
| `pipeline.port.duplicate-name`, `missing-name` | `contract.port.duplicate-name`, `contract.port.invalid-name` | ported (at contract level) |
| `pipeline.node.invalid-activation-mode` | closed enum → `yaml.schema` | ported |
| `pipeline.node.invalid-backpressure`, `invalid-parallelism` | closed enum → `yaml.schema`; `graph.node.invalid-parallelism` | ported (#24, #25) |
| value/select/loop/binding codes (`select-*`, `loop-*`, `invalid-binding-*`, `literal-type-mismatch`, `unresolvable-value`, `invalid-value-*`, `invalid-schema`) | `graph.value.*`, `graph.select.*`, `graph.loop.*`, `graph.binding.*`, `graph.primitive.*`, `graph.edge.value-type-mismatch` | ported (#25) |
| `pipeline.edge.invalid-loop-target` | `graph.edge.invalid-loop-target` (`edges.loop`, ids only) | ported (#25) |

## Execution, data plane, recovery

| Behaviour | .NET tests | Rust | Status |
|---|---|---|---|
| Topological serial execution | `GraphTopologySorterTests`, `PipelineGraphExecutorTests` | `mvf-engine` executor | ported — DAG, sources-first, control gating, primitives (#25) |
| Fork / if / switch / select / value / loop primitives | `*PrimitiveNodeRunnerTests`, `LoopAndRunningStateGraphTests`, `ValueAndSelectGraphTests` | `mvf_graph::primitive`, `mvf_engine::primitives`, `mvf_engine::live` | ported (#25) — demo outputs byte-identical with .NET ([stage 12](stages/12.md)); values travel on data edges (documented) |
| Binding store, pre-pass, environment resolver | `BindingPrePassTests`, `ModuleBindingGraphTests` | `mvf_engine::bindings`, `RunOptions`/`--bindings` | ported (#25) — no interactive prompt (unattended host) |
| Live tuning, pause/resume | `ValuePrimitiveNodeRunnerTests`, `LoopAndRunningStateGraphTests` | `ControlHandle::{set_value, configure, pause, resume}`, update-plan steps | ported (#25) — no TUI; scripted or API-driven |
| Pipelined executor, joins, per-node parallelism | `PipelinedGraphExecutorTests` | `mvf_engine::pipelined` (`--mode pipelined`) | ported (#25) — plus loop/value/select/on-demand/live replacement in pipelined mode ([stage 13](stages/13.md)) |
| Graph-derived arena sizing | `DataPlaneSizing` | `pipelined::arena_slots_for` | ported (#25) |
| Shared-memory arena, refcounts, fan-out | `SharedMemoryArenaTests`, `PipelineGraphExecutorDataPlaneTests` | `mvf_memory::Arena` + `Payload::Arena` | ported — plus generation-checked handles and copy accounting |
| Backpressure stall/drop | `PipelineGraphExecutorBackpressureTests` | `mvf_engine::recovery`, `mvf_engine::pipelined` | ported (#24, #25) — pipelined `stall` waits for a release |
| Supervised worker, readiness, warm pool | `SupervisedWorkerTests`, `WorkerReadinessTests`, `WarmWorkerPoolTests` | `mvf-worker` (`MVF_WARM_SPARES`) | ported — readiness, deadlines, kill, reaping, restart (#24), warm pool (#28) |
| Orphan prevention when the host dies | — (cooperative stdin EOF) | `mvf_worker::spawner` | new (#28) — Linux PDEATHSIG, Windows Job Object; macOS cooperative only |
| Per-node arena isolation | — | `spec.memory.arena: private`, `MVF_ISOLATE_ARENAS` | new (#28) |
| Worker logs / observability | `WorkerLoggingTests`, `WorkerObservabilityTests` | `HostServices.logs` | partial — logs, stderr, supervisor events; no RPC latency metrics |
| Epoch checkpoint / restore / resume | `PipelineGraphExecutorCheckpointTests`, `CheckpointResumeTests`, `WorkerCheckpointTests` | `run.checkpoint`, worker `checkpoint`/`restore` | ported (#24) — atomic epochs, durable resume, worker state |
| Node failure policy (fail / restart) | `NodeResilienceTests`, `SourceFailureTests` | `onError`, `run.onError` | ported (#24) — plus explicit `skip`; Rust default `fail` for all roles |
| Live config (`configure`) | `LiveConfigureTests` | worker `reconfigure` over protocol `configure`, else reopen | ported (#25) |
| Egress / TUI / web studio | `Egress*Tests`, `GlyphsAsciiTests` | — | out of scope (adapters at the edges) |

## New in Gate B (no .NET equivalent)

| Behaviour | Rust | Status |
|---|---|---|
| OCI image store (import/verify/export, Docker interop) | `mvf-oci`, `mvf-rs image …` | new — stage 07 |
| Daemonless OCI node execution, default-deny sandbox, `spec.permissions` | `mvf-container` | new — Linux only (stage 08) |
| GPU compute nodes, device-resident payloads | `mvf-compute` | new — tested on Apple M4/Metal (stage 09) |
| PLC simulator with duplicate-commit detection | `plc-sim` builtin | new — stage 10 |

Still missing: worker RPC latency metrics; interactive prompt/TUI and egress are out of scope
(adapters at the edges).
