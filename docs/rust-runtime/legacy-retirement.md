# Retiring the legacy .NET runtime (#34): inventory and gate

**Status (v1.1.0):** the transition has started, and nothing has been removed.

- `mvf-rs` is the canonical runtime, and the release ships it beside the unchanged legacy CLI.
- [migration-guide.md](migration-guide.md) covers conversion (`mvf-rs import`), validation and
  rollback.
- Removal waits for the gate below. #34 itself says it does not authorize immediate removal.

## Inventory

| Project | Role | Classification |
|---|---|---|
| `src/core/Mvf.Graph`, `src/core/Mvf.Abstractions` | contract types; packed to NuGet with the SDK | **retain** (the SDK depends on them) |
| `src/sdk/dotnet/Mvf.Worker` (+ `examples/`) | .NET worker SDK for the Rust runtime (#32) | **retain**: the supported way to run C# under Rust |
| `src/sdk/dotnet/Mvf.Sdk` | in-process module SDK of the legacy engine | **retain** while in-process modules exist; migrate modules to `Mvf.Worker` |
| `src/engine/Mvf.Engine` | legacy graph engine | **remove after the gate** |
| `src/cli/Mvf.Cli` | legacy CLI (TUI, egress, prompts) | **remove after the gate**; the rollback path until then |
| `src/hosting/Mvf.Hosting.Worker` | the legacy engine's worker host | **remove after the gate** |
| `src/transports/Mvf.Transport.SharedMemory` | the legacy engine's shared-memory transport | **remove after the gate** (Rust: `mvf-memory`; .NET workers: `Mvf.Worker`'s own arena) |
| `src/egress/Mvf.Egress` | live egress from the legacy engine | **migrate**: no Rust equivalent yet. It blocks removal for any line that uses egress |
| `src/egress/Mvf.Egress.Client`, `clients/mvf-egress-client` | egress client SDKs | **retain** while egress exists |
| `modules/…FolderSource`, `modules/…DatasetWriter` | in-process modules | **remove after the gate**: Rust builtins `folder-source` and `dataset-writer` match (byte-identical datasets) |
| `modules/…CognexCamera`, `…SyntheticCamera`, `…BlackScreenCheck`, `…DarkFrameFilter`, `modules/dotnet-brightness-gate` | in-process modules | **migrate** to `Mvf.Worker` + `node.yaml` before removal (the importer reports them MISSING) |
| `modules/py-*` | Python modules with `node.yaml` | **retain**: both runtimes run them |
| `tests/Mvf.Engine.Tests` | legacy engine tests | **keep as reference** until removal; their behaviour is tracked in [parity-matrix.md](parity-matrix.md) |
| `tools/Mvf.SchemaExporter` | JSON schema of `pipeline.json` | **remove after the gate** |
| `packages/*/pipeline.json` | legacy packages | **retain** as parity fixtures (`RUST_SOURCE=import rust/scripts/parity-demos.sh`) |
| CI `.NET build + test`, release job `cli` (legacy CLI) | | **keep until removal**, so the rollback binary is still built |

## Gate (from #34)

| Prerequisite | State |
|---|---|
| Functional parity for primitives, policies, checkpoints, lifecycle, bindings, payloads, live replacement | done for DAG graphs ([parity-matrix.md](parity-matrix.md)); demo datasets byte-identical, also when converted by `mvf-rs import` |
| IPC (#32) and SDKs (Python, C++, .NET) pass | done; CI runs all three against the Rust runtime |
| Production capabilities and platforms | Windows, Linux, macOS native and worker: CI PASS. #26 (Linux + GPU host) and #27 (Windows OCI): **BLOCKED** on hardware. Egress: **not ported** |
| First usable Rust release, pinned for the Rotary migration | **v1.1.0** (this release) |
| Rotary validation on edge hardware: parity, performance, shadow, soak, recovery, tested rollback | **open**: the maintainer's real legacy pipeline is the next step |
| Stable release, migration guide, accepted gaps | guide: [migration-guide.md](migration-guide.md); gaps above |
| Maintainer approval after evidence | **open** |

After the gate:

1. Remove the "remove" rows and their CI and release jobs.
2. Make the README point only at `mvf-rs`.
3. Keep the v1.0.0 and v1.1.0 tags and their release artifacts as the immutable rollback.
