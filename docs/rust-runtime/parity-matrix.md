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
| `runtime: dotnet \| python \| node` | `ModuleManifest.Runtime` | `artifacts[].backend` + `worker.runtime: python \| executable` | partial — `node` runtime not mapped; .NET modules → worker/OCI (#13, #18) |
| `lifecycle: resident \| on-demand` | `ModuleManifest.Lifecycle` | `spec.lifecycle.activation` | ported (schema) |
| `backpressure: stall \| drop` | `ModuleManifest.Backpressure` | — | not yet (bounded admission in #14) |
| `maxParallelism` | `ModuleManifest.MaxParallelism` | — | not yet (serial executor only) |
| Readiness budget (`hello ready:false` → `ready`) | `protocol/README.md`, `WorkerReadinessTests` | `spec.lifecycle.readinessTimeoutMs` | ported (schema); enforcement in #13 |
| Typed payload descriptor (192-byte header, `MVF1`) | `PayloadDescriptor.cs` | — | not yet (#14) |
| Isolation / resource / memory / placement policy | — | `spec.isolation`, `spec.resources`, `spec.memory`, `spec.execution` | new |
| Live replacement + versioned state | — | `spec.lifecycle.replacement`, `spec.lifecycle.state` | new (schema); behaviour in #15 |
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
| `pipeline.node.invalid-backpressure`, `invalid-parallelism` | — | not yet |
| value/select/loop/binding codes (`select-*`, `loop-*`, `invalid-binding-*`, `literal-type-mismatch`, `unresolvable-value`, `invalid-value-*`, `invalid-schema`) | — | not yet (primitives out of Gate A scope) |
| `pipeline.edge.invalid-loop-target` | — | not yet (loop primitive) |

## Execution, data plane, recovery

| Behaviour | .NET tests | Rust | Status |
|---|---|---|---|
| Topological serial execution | `GraphTopologySorterTests`, `PipelineGraphExecutorTests` | `mvf-engine` executor | partial — DAG, sources-first, control gating; no primitives |
| Fork / if / switch / select / value / loop primitives | `*PrimitiveNodeRunnerTests` | — | not yet |
| Pipelined executor, joins, per-node parallelism | `PipelinedGraphExecutorTests` | — | not yet (out of Gate A) |
| Shared-memory arena, refcounts, fan-out | `SharedMemoryArenaTests`, `PipelineGraphExecutorDataPlaneTests` | — | not yet (#14) |
| Backpressure stall/drop | `PipelineGraphExecutorBackpressureTests` | — | not yet (#14 bounded admission) |
| Supervised worker, readiness, warm pool | `SupervisedWorkerTests`, `WorkerReadinessTests`, `WarmWorkerPoolTests` | — | not yet (#13) |
| Worker logs / observability | `WorkerLoggingTests`, `WorkerObservabilityTests` | — | not yet (#13) |
| Epoch checkpoint / restore / resume | `PipelineGraphExecutorCheckpointTests`, `CheckpointResumeTests`, `WorkerCheckpointTests` | — | not yet |
| Node failure policy (fail / restart) | `NodeResilienceTests`, `SourceFailureTests` | — | not yet |
| Live config (`configure`) | `LiveConfigureTests` | — | not yet |
| Egress / TUI / web studio | `Egress*Tests`, `GlyphsAsciiTests` | — | out of scope (adapters at the edges) |
