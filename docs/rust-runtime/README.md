# Rust runtime research (issue #10)

Research branch `research/rust-runtime`. Implements the ordered backlog of
[#10](https://github.com/hmetgundogdu/machine-vision-fabric/issues/10): Gate A (#11–#16) and Gate B
(#17–#20), then the follow-ups #24–#28 (stage 11: #24; stages 12–13: #25), the Network Fabric #23 (stages 16–17), the Display Fabric #30 (stages 18–19), the Storage Fabric #31 (stage 20) and worker protocol v2 #32 (stage 21; stage 22: the C++ and .NET SDKs; stage 23: direct receive on Windows), and runtime management #33 (stage 24: local observation). The shipping .NET engine is **unchanged** and remains the behavioural reference.

## Layout decision

The Rust workspace lives in **`rust/`** at the repository root, not at the root itself. The root
already holds `Mvf.slnx`, `Directory.Build.props`, `global.json` and `build/`; a root `Cargo.toml`
would mix two build systems and make `cargo` and `dotnet` tooling trip over each other's folders.
`rust/` keeps the workspace self-contained (`rust/Cargo.toml`, `rust/Cargo.lock`,
`rust/rust-toolchain.toml`) and lets CI run it as an isolated job. Moving it to the root later is a
mechanical change once the .NET host retires.

| Crate | Slice | Purpose |
|---|---|---|
| `mvf-contract` | #11 | Universal Node Contract: strict YAML loader, typed model, validation, capability negotiation. No host dependencies. |
| `mvf-graph` | #12, #25 | Pipeline document, typed graph validation (separate data/control/loop edges), primitives with synthesized contracts, value types and JSON Schema subset, deterministic order. Pure. |
| `mvf-engine` | #12– | Single scheduling authority: typed values, node handles, backends, builtins, serial and pipelined executors (#25), recovery (#24), bindings and live operation (#25). |
| `mvf-memory` | #13–#14 | Descriptor codec (compatible with .NET/Python/C++), engine-owned file-backed arena, leases. |
| `mvf-worker` | #13 | Supervised stdio worker backend (protocol v1): readiness, deadlines, kill, exit classification, logs. |
| `mvf-oci` | #17 | Offline OCI layout / Docker archive import, verified content store, safe unpack, export, layer builder. |
| `mvf-container` | #18 | Daemonless OCI backend (runc/crun), default-deny sandbox spec, packaging, generic workloads. |
| `mvf-compute` | #19 | wgpu compute backend (Metal/Vulkan/DX12): WGSL kernels, device-resident payloads, same-device zero-copy hand-off. |
| `mvf-display` | #30 | Display Fabric core, no window library: session probe (interactive / service / headless), shared-memory surface v1 (latest frame, CRC-verified, input ring), per-user surface registry. |
| `mvf-display-host` | #30 | Optional `mvf-display-host` for the user's session (winit + softbuffer), `mvf-cube` (Rust 3D renderer node), `mvf-native-window` (native-UI PoC). Nothing else links it. |
| `mvf-ipc` | #32 | Worker protocol v2: binary framing and bodies (host and Rust children), the fixture child, benchmarks against v1 and fault injection. |
| `mvf-storage` | #31 | Storage Fabric: provisioning of persistent, shared, host-bind and digest-pinned artifact volumes and scratch; path safety; retention, sweep and accounting. The OS does the I/O. |
| `mvf-cli` | #12– | `mvf-rs` native CLI: `contract`, `validate`, `run`, `host`, `image`, `volume`. |

## Commands

Run from `rust/`. The toolchain is pinned by `rust-toolchain.toml` (1.95.0).

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo test --workspace --locked
```

## Gate A in one command

```bash
rust/scripts/gate-a.sh
```

Runs fmt, clippy, the C++ SDK interop fixture build, all tests and the self-verifying live-update
demo. Windows: `pwsh rust/scripts/gate-a.ps1`. Baseline against the .NET engine:
`rust/scripts/bench-baseline.sh`.

## Documents

- [p0-report.md](p0-report.md) / [gate-b-report.json](gate-b-report.json) — P0 verdicts (Gate B; P0.1 re-evaluated after #24)
- [adr-0002-p0-runtime.md](adr-0002-p0-runtime.md) — OCI, GPU and the decision after Gate B
- [adr-0003-network-fabric.md](adr-0003-network-fabric.md) — per-node network policy, enforcement matrix, listener handover (#23)
- [adr-0004-display-fabric.md](adr-0004-display-fabric.md) — presentation profiles, session matrix, managed surfaces, display host (#30)
- [adr-0005-storage-fabric.md](adr-0005-storage-fabric.md) — volumes, bindings, enforcement matrix, artifacts and lifecycle (#31)
- [adr-0006-worker-ipc-v2.md](adr-0006-worker-ipc-v2.md) — control protocol v2: transport/schema evidence and decision (#32)
- [adr-0007-runtime-management.md](adr-0007-runtime-management.md) — runtime management: local read-only feed, remote through a separate relay (#33)
- [p0-thresholds.md](p0-thresholds.md) — go/no-go thresholds fixed before the final runs
- [competitive-landscape.md](competitive-landscape.md) — closest alternatives, pinned versions

- [gate-a-report.md](gate-a-report.md) / [gate-a-report.json](gate-a-report.json) — Gate A result
- [adr-0001-gate-a-foundation.md](adr-0001-gate-a-foundation.md) — contract, execution and memory boundaries
- [node-contract.md](node-contract.md) — Universal Node Contract v1alpha1 specification
- [graph-primitives.md](graph-primitives.md) — fork/if/switch/select/value/loop, bindings, live operation, differences from .NET
- [parity-matrix.md](parity-matrix.md) — existing .NET behaviour vs the Rust runtime
- [stages/](stages/) — one report per slice with commands, results and PASS/FAIL/BLOCKED
