# Retiring the legacy .NET runtime (#34)

**Status (v2.0.0): archived.**

- **Who decided:** the maintainer asked for the legacy runtime to be removed from `main` and
  archived.
- **Where it went:**
  - **Code:** branch `archive/dotnet-legacy` and tag `dotnet-legacy-final`, both at `67d9d75`.
    Full history; nothing was rewritten.
  - **Last release that ships it:** v1.1.0, with the legacy CLI `mvf-cli-<os>`, the
    `MachineVisionFabric.{Graph,Abstractions,Sdk}` 1.1.0 packages and the Rust runtime as
    `mvf-rs`. It stays as the immutable rollback.
- **What ships instead:** `main` builds, tests and releases only `mvf` and the SDKs. The CLI was
  renamed from `mvf-rs` to `mvf` in v2.0.0.

## What moved where

| Project | Role | Now |
|---|---|---|
| `src/engine/Mvf.Engine` | legacy graph engine | **archived** |
| `src/cli/Mvf.Cli` | legacy CLI (TUI, egress, prompts) | **archived**. `mvf` is the CLI; the legacy binary is in v1.1.0 |
| `src/hosting/Mvf.Hosting.Worker` | the legacy engine's worker host | **archived** |
| `src/transports/Mvf.Transport.SharedMemory` | the legacy engine's shared memory | **archived** (Rust: `mvf-memory`; .NET workers: `Mvf.Worker`'s arena) |
| `src/egress/*`, `clients/mvf-egress-client` | live egress and its clients | **archived**: no Rust equivalent yet (see Gaps) |
| `src/core/Mvf.Graph`, `src/core/Mvf.Abstractions`, `src/sdk/dotnet/Mvf.Sdk` | in-process module SDK | **archived**: only the legacy engine loads in-process modules. The last packages are 1.1.0 |
| `modules/MachineVisionFabric.Integrations.*`, `modules/dotnet-brightness-gate` | in-process modules | **archived**. Folder source and dataset writer are `mvf` builtins (byte-identical datasets); the others need a `Mvf.Worker` port |
| `modules/py-tunable-transformer` | Python module with only a legacy `module.json` | **archived** (no `node.yaml`) |
| `tests/Mvf.Engine.Tests`, `tools/Mvf.SchemaExporter`, `publish.ps1` | legacy tests and tooling | **archived**. The behaviour is tracked in [parity-matrix.md](parity-matrix.md) |
| `src/sdk/dotnet/Mvf.Worker` (+ `examples/`) | .NET worker SDK for `mvf` (#32) | **kept**, and published as `MachineVisionFabric.Worker` |
| `src/sdk/python`, `src/sdk/cpp`, `modules/py-*` | worker SDKs and modules | **kept** |
| `packages/*/pipeline.json` | legacy packages | **kept** as import and parity fixtures |
| CI `.NET build + test`, release jobs `cli` (legacy) and NuGet (in-process SDK) | | **replaced**: the CI job builds and packs the worker SDK; the release ships `mvf` and `MachineVisionFabric.Worker` |

## Comparing against the legacy engine

`rust/scripts/parity-demos.sh` and `rust/scripts/bench-baseline.sh` still compare with the legacy
engine. They check out `dotnet-legacy-final` into `.legacy-dotnet/` (git-ignored, or set
`LEGACY_ROOT`) and build it there.

```text
$ RUST_SOURCE=import DEMOS="inspection-demo loop-demo value-demo multilang-demo" rust/scripts/parity-demos.sh
inspection-demo          60         60  IDENTICAL
loop-demo                24         24  IDENTICAL
value-demo               24         24  IDENTICAL
multilang-demo            8          8  IDENTICAL
PARITY PASS
```

## Gaps the archive leaves

- **Realtime egress** (`--egress tcp|ws|udp`) is not ported. A line that needs it runs the
  v1.1.0 legacy CLI until it is.
- **.NET in-process integrations** (Cognex camera, dark-frame filter, black-screen check,
  synthetic camera) need a `Mvf.Worker` port. `mvf import` names them.
- **No TUI or interactive prompts.** Observation goes through `mvf run --manage` and `mvf manage`.
- **Blocked on hardware:** #26 (Linux + GPU host) and #27 (Windows OCI).
- **Rotary validation** on edge hardware (parity, performance, shadow, soak, recovery, rollback)
  is the maintainer's real-pipeline migration, in progress. The v1.1.0 legacy CLI is its
  rollback.
