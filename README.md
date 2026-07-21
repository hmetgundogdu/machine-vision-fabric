# MachineVisionFabric

MachineVisionFabric is an open-source edge vision platform for headless dataset collection, device integration, PLC-gated capture, and future machine-vision execution on edge devices.

The repository now distinguishes between:

- the current package-driven composition runtime
- the future typed pipeline graph runtime

## Product Boundary

- `MachineVisionFabric` is the platform product.
- camera SDK adapters, PLC adapters, and customer-specific logic are not platform code
- graph or pipeline authoring UI is a future product layer, not the current MVP

The repository now reflects that split explicitly:

```text
MachineVisionFabric/
|-- src/                  platform core, layered so the engine stays minimal
|   |-- core/             Mvf.Graph (typed graph model + validation), Mvf.Abstractions (contracts)
|   |-- engine/           Mvf.Engine (scheduler + node runners)
|   |-- sdk/dotnet/       Mvf.Sdk (.NET module authoring)
|   `-- cli/              Mvf.Cli (headless host + ASCII TUI)
|-- modules/              .NET integration modules (Cognex camera, filters, dataset writer)
|-- packages/             runnable pipeline packages (pipeline.json)
|-- tools/                Mvf.SchemaExporter
|-- tests/
`-- docs/                 architecture + roadmap (see docs/roadmap.md)
```

The **core** (`src/core`) knows only what a pipeline is (typed graph) and what a node
contract is; transports, module hosts and language SDKs attach at the edges so the core
stays small. `modules/` holds pluggable integration modules; `packages/` holds the
pipelines that compose them. See `docs/roadmap.md` for the architecture and roadmap.

## Current Direction

The project is intentionally `headless-first` and `dataset-first`.

The first MVP goal is:

- reliable runtime bootstrap
- package and profile loading
- dataset session creation
- source and gate resolution through `.NET` modules
- simulator-driven capture without hardware

At the same time, the next architectural layer is now defined as a typed graph model with:

- embedded engine-owned primitive nodes such as `if`, `switch`, `fork`, and `loop`
- external SDK-based work nodes such as camera, PLC, inference, stream, and storage integrations

UI comes later.

## Verified Status

As of `2026-07-16`, the current MVP is verified to:

- collect dataset frames into session folders
- write per-frame metadata plus `session.json`
- resolve a frame source from an external `.NET` integration module
- resolve a product presence gate from an external `.NET` integration module
- stream frames through the new `IFrameSourceSession` contract
- capture from both file-backed and in-memory frame envelopes
- capture `pre/post trigger` frame windows around the first positive gate event
- skip capture when the gate says the product is not present

As of `2026-07-17`, the typed pipeline graph is the primary execution model, driven by
`execute-graph`. The repository ships one end-to-end graph package:

- `packages/cognex-dark-capture` — Cognex auto-trigger capture that
  saves every frame and branches very dark frames into a separate dataset.

The runtime discovers integration modules under `modules`
(Cognex camera source, dark-frame filter, black-screen check, dataset writer) and exposes
a typed inspection surface for resolved pipelines and SDK module metadata.

## Run

Build the platform and the integration modules:

```powershell
dotnet build Mvf.slnx -v minimal
```

`Mvf.slnx` includes the platform, the modules and the tools. The CLI resolves its default
paths relative to the repository root, so the commands below work with no flags when run
from a clone. `CLI` is `src\cli\Mvf.Cli\bin\Debug\net10.0\Mvf.Cli.dll`.

List the discovered integration modules:

```powershell
dotnet $CLI modules
```

List the runnable packages (`graph` = pipeline.json package):

```powershell
dotnet $CLI packages
```

Validate the shipped pipeline graph:

```powershell
dotnet $CLI validate-pipeline --path packages\cognex-dark-capture\pipeline.json
```

Run the default pipeline (`packages\cognex-dark-capture`). Add
`--no-tui` for plain output and `--max-cycles <n>` to stop after n cycles:

```powershell
dotnet $CLI execute-graph
dotnet $CLI execute-graph --no-tui --max-cycles 1
```

Run a different package or module/integration root explicitly:

```powershell
dotnet $CLI execute-graph --package <package-dir> --integrations-root <integrations-dir>
```

### Self-contained deploy

`publish.ps1` assembles the CLI, the integration modules, and the packages into a single
folder with `appsettings.json` patched for that layout:

```powershell
./publish.ps1                     # -> publish/mvf
cd publish/mvf
./Mvf.Cli execute-graph --package packages/cognex-dark-capture
```

## Integrator Direction

External developers should:

- build `.NET` integration modules against `Mvf.Sdk`
- load them through the platform runtime
- validate config with exported JSON schema
- keep vendor SDK code outside `src/`

That means a real camera adapter belongs in its own project under `modules/` or another external solution, not inside the platform core.

## Documents

- [Architecture Foundation](docs/architecture-foundation.md)
- [Platform Product Boundary](docs/platform-product-boundary.md)
- [Pipeline Graph Foundation](docs/pipeline-graph-foundation.md)
- [Integration SDK Strategy](docs/integration-sdk-strategy.md)
- [SDK Quickstart](docs/sdk-quickstart.md)
- [Dataset-First MVP Roadmap](docs/dataset-first-mvp-roadmap.md)
- [Session Handoff](docs/session-handoff-2026-07-16.md)
