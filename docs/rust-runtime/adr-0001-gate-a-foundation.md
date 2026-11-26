# ADR-0001 — Rust core foundation: contract, execution and memory boundaries (Gate A)

- **Status:** Accepted for the research branch `research/rust-runtime`. Not a production migration
  decision; the .NET engine remains the shipping implementation and the behavioural reference.
- **Date:** 2026-11-26
- **Issues:** #10 (RFC, approved target: Pure Rust core), #11–#16 (Gate A)
- **Evidence:** [stages/01–06](stages/), [gate-a-report.md](gate-a-report.md),
  [gate-a-report.json](gate-a-report.json)

## Context

#10 approved a Pure Rust core: one Rust workspace holds the graph engine and the node runtime; C# is
a node/SDK language, not a mandatory host. Gate A is the bounded experiment that has to show the
foundation works — a typed graph, one scheduling authority, pluggable execution backends, a
shared-memory data plane compatible with the existing SDKs, and live node replacement — before any
P0 work (OCI, GPU) builds on it.

## Decisions

### 1. Workspace and crate boundaries

`rust/` (not the repository root, which belongs to the .NET build) with six crates whose
dependencies only point downwards:

```text
mvf-cli ──► mvf-worker ──► mvf-engine ──► mvf-graph ──► mvf-contract
                    └────────────► mvf-memory ◄──┘
```

`mvf-contract` and `mvf-graph` are pure (no processes, files only through a resolver trait, no shared
memory). `mvf-engine` is the **single graph scheduling authority**. Backends (`mvf-worker`, later
OCI/compute) plug in below it and never see graph semantics.

### 2. Contract boundary — Universal Node Contract `mvf.dev/v1alpha1`

A node is described by typed ports, ordered artifacts per backend, placement with an explicit
fallback, an **isolation level** (`none < process < sandbox < vm`, each documented with what it does
not guarantee), memory domain + zero-copy requirement, lifecycle (activation, readiness/drain/execute
budgets, replacement mode, versioned state) and resource limits with an enforcement policy. YAML is
parsed by one strict, bounded loader (no anchors/aliases/tags, duplicate keys rejected, unknown
fields rejected with a path). Backend capability **negotiation fails closed**: an unmet mandatory
requirement is a structured rejection, an unmet preference is a visible placement note.

### 3. Graph boundary

Data and control edges are separate lists and separate collections; a single untyped edge list is
rejected. DAG only, one edge per input (no implicit joins), fan-out allowed, deterministic order
(sources first, then topological order with declaration-index tie-break). Every emitted value is
re-validated against the producer's declared port at runtime.

### 4. Execution boundary

Every backend produces a `NodeHandle` with the same lifecycle (`start` with a readiness budget,
`execute` with cycle id, generation, deadline and cancellation, `drain`, idempotent `stop`, versioned
state export/import). The executor never branches on the backend. Failure classes are part of the
contract: managed error, caught panic, process exit/signal, timeout, cancellation, protocol violation,
unsupported request, invalid config/output, resource exhaustion.

| Backend | Isolation | Cancellation | Notes |
|---|---|---|---|
| in-process | `none` (engine failure domain) | cooperative | trusted builtins; panics caught, handle poisoned |
| worker (stdio protocol v1) | `process` — crash/hang containment, **not** security | enforceable (kill) | existing Python and C++ SDK modules run unchanged |

### 5. Memory boundary

The engine owns a file-backed arena with the **unchanged v1 ABI** (192-byte descriptor header,
payload at +192, `MVF_ARENA_PATH`, `shm.offset`), so no SDK changed. Payloads are either `Arc` heap
bytes or reference-counted, generation-checked arena leases; all bookkeeping is host-side. Producers
write their own outputs in place (*transform writes*); hand-offs pass leases or offsets; transfer
copies are counted separately. Trust assumption, documented: workers map the arena read-write, so a
native worker can read any slot and write its inputs — the host re-validates headers and can detect
(not prevent) input mutation.

### 6. Live replacement

Replacement is a scheduler operation at cycle boundaries: validate (ports, mode, state schema,
negotiation), prepare off the critical path, drain + migrate state + switch generation atomically,
keep the previous generation warm for a probation window, roll back and replay the failed cycle on a
candidate failure, then commit. Preparation and cut-over times are reported separately; no universal
zero-downtime claim.

## Measured (single machine, not a benchmark campaign)

See [gate-a-report.md](gate-a-report.md#baseline-against-the-net-engine) for the full table and
procedure (`rust/scripts/bench-baseline.sh`). Matched workloads (same frames, same Python modules,
same topology, serial executors) on an Apple M4; numbers are from one run each and only indicate
direction.

## Unresolved (explicitly not solved by Gate A)

- Graph parity: fork/if/switch/select/value/loop primitives, joins, pipelined executor and per-node
  parallelism, `on-demand` activation, restart failure policy, stall/drop backpressure, epoch
  checkpoint/restore, live `configure` bindings, egress — all present in .NET, absent here.
- Worker robustness: no warm pool, no restart-on-crash; orphan prevention when the **host** is
  SIGKILLed relies on SDK children exiting on stdin EOF (no `PR_SET_PDEATHSIG`/Job Objects yet).
- Data plane: single slot size; no per-worker arenas or sealing, so no confidentiality between
  workers sharing an arena; no snapshot of arena contents.
- Replacement: sources cannot be replaced; stateful replacement only for in-process nodes; no
  shadow evaluation; exclusive devices are not modelled.
- Contract: `v1alpha1` is not frozen; WASM/compute artifacts are schema-only until a backend exists.

## Decision on proceeding

Gate A **passes** (see the gate report): every required check is green on macOS locally and on
Linux/Windows/macOS in CI, with real Python and C++ workers. The foundation is sufficient to start
the P0 work of Gate B (#17–#20) **on the research branch**. This ADR does not authorize merging into
`main`, retiring the .NET host, releasing, or claiming parity beyond the matrix in
[parity-matrix.md](parity-matrix.md).
