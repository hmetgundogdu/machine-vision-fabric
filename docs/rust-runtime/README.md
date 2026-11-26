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

## Documents

- [node-contract.md](node-contract.md) — Universal Node Contract v1alpha1 specification
- [parity-matrix.md](parity-matrix.md) — existing .NET behaviour vs the Rust runtime
- [stages/](stages/) — one report per slice with commands, results and PASS/FAIL/BLOCKED
