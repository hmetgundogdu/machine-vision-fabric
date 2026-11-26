# Rust runtime research (issue #10)

Research branch `research/rust-runtime`. Implements the ordered backlog of
[#10](https://github.com/hmetgundogdu/machine-vision-fabric/issues/10): Gate A (#11–#16) and Gate B
(#17–#20). The shipping .NET engine is **unchanged** and remains the behavioural reference.

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
| `mvf-graph` | #12 | Pipeline document, typed graph validation (separate data/control edges), deterministic order. Pure. |
| `mvf-engine` | #12– | Single scheduling authority: typed values, node handles, backends, builtins, executor. |
| `mvf-memory` | #13–#14 | Descriptor codec (compatible with .NET/Python/C++), engine-owned file-backed arena, leases. |
| `mvf-worker` | #13 | Supervised stdio worker backend (protocol v1): readiness, deadlines, kill, exit classification, logs. |
| `mvf-oci` | #17 | Offline OCI layout / Docker archive import, verified content store, safe unpack, export, layer builder. |
| `mvf-cli` | #12– | `mvf-rs` native CLI: `contract`, `validate`, `run`, `host`. |

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

- [gate-a-report.md](gate-a-report.md) / [gate-a-report.json](gate-a-report.json) — Gate A result
- [adr-0001-gate-a-foundation.md](adr-0001-gate-a-foundation.md) — contract, execution and memory boundaries
- [node-contract.md](node-contract.md) — Universal Node Contract v1alpha1 specification
- [parity-matrix.md](parity-matrix.md) — existing .NET behaviour vs the Rust runtime
- [stages/](stages/) — one report per slice with commands, results and PASS/FAIL/BLOCKED
