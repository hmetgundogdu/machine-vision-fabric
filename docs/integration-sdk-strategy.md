# Integration SDK Strategy

## Goal

External developers build integration modules against a stable public contract surface —
in **.NET, Python, or C++** — without editing platform runtime code.

## SDK surfaces

One language-agnostic module protocol (stdio control plane + shared-memory data plane, see
[`protocol/README.md`](../protocol/README.md)) is exposed through three SDKs:

| Language | SDK | Package / entry |
|---|---|---|
| .NET | `Mvf.Sdk` (`src/sdk/dotnet`) | NuGet `MachineVisionFabric.Sdk` |
| Python | `mvf_sdk` (`src/sdk/python`) | wheel `mvf-sdk` |
| C++ | `mvf/sdk.hpp` (`src/sdk/cpp`) | `libmvf_sdk.{so,dylib}` / `mvf_sdk.dll` |

The .NET SDK wraps the lower-level contracts in `Mvf.Abstractions` and `Mvf.Graph`, which
define: integration-module contracts, frame source / processor / classifier / sink /
gate contracts, package contracts, and dataset-capture contracts. None of the SDKs define
engine-owned control-flow primitives (`if`, `switch`, `fork`, `loop`) — those are part of
graph execution semantics and stay in the engine.

## Runtime model

The runtime:

- loads .NET integration assemblies in isolation (`AssemblyLoadContext`);
- launches Python/C++ modules as out-of-process workers over the module protocol;
- keeps vendor dependencies out of the platform core;
- validates configuration through platform-owned JSON schema (exported via
  `System.Text.Json`, see `tools/Mvf.SchemaExporter`);
- stays deployable without any specific camera-vendor dependency.

## Typed metadata rule

Modules must be fully typed and inspectable — a hard requirement so future studio tooling
can inspect a module catalog, show valid connections, reuse a module across pipelines, and
validate composition before execution. Every module exposes:

- a typed config contract,
- a typed capability kind,
- typed input port metadata,
- typed output port metadata.

## Boundary rule

- Platform assemblies live under `src/`.
- Example modules live under `modules/` (`py-invert-transformer`, `dotnet-brightness-gate`)
  and `src/sdk/cpp/examples/`.
- A real vendor/customer adapter is a separate module that implements the contracts and is
  loaded by the runtime — never a patch to the core or a vendor reference inside `src/`.

Practical split: **embedded primitive = engine feature**, **integration module = SDK
extension**.

See [sdk-quickstart.md](sdk-quickstart.md) for how to author one.
