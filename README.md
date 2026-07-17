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
|-- src/                  platform product
|   |-- MachineVisionFabric.Contracts/
|   |-- MachineVisionFabric.Core/
|   |-- MachineVisionFabric.Sdk/
|   |-- MachineVisionFabric.Runtime/
|   |-- MachineVisionFabric.Storage/
|   |-- MachineVisionFabric.Cli/
|   `-- MachineVisionFabric.Host/
|-- real-world-projects/  project-specific integrations and scenarios
|   |-- integrations/     .NET integration modules (Cognex camera, filters, writers)
|   |-- packages/         runnable pipeline packages
|   `-- MachineVisionFabric.RealWorld.slnx
|-- tools/                platform-owned tooling
|   `-- MachineVisionFabric.SchemaExporter/
|-- tests/
`-- docs/
```

`src/` is the platform.
`real-world-projects/` is where project-specific camera and scenario work lives —
integration modules and the pipeline packages that compose them. It is intentionally
kept in the same repository, but under its own folder and solution boundary.

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

- `real-world-projects/packages/cognex-dark-capture` — Cognex auto-trigger capture that
  saves every frame and branches very dark frames into a separate dataset.

The runtime discovers integration modules under `real-world-projects/integrations`
(Cognex camera source, dark-frame filter, black-screen check, dataset writer) and exposes
a typed inspection surface for resolved pipelines and SDK module metadata.

## Run

Build the platform and the integration modules:

```powershell
dotnet build MachineVisionFabric.slnx -v minimal
dotnet build real-world-projects\MachineVisionFabric.RealWorld.slnx -v minimal
```

The CLI resolves its default paths relative to the repository root, so the commands below
work with no flags when run from a clone. `CLI` is
`src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll`.

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
dotnet $CLI validate-pipeline --path real-world-projects\packages\cognex-dark-capture\pipeline.json
```

Run the default pipeline (`real-world-projects\packages\cognex-dark-capture`). Add
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
./MachineVisionFabric.Cli execute-graph --package packages/cognex-dark-capture
```

## Integrator Direction

External developers should:

- build `.NET` integration modules against `MachineVisionFabric.Sdk`
- load them through the platform runtime
- validate config with exported JSON schema
- keep vendor SDK code outside `src/`

That means a real camera adapter belongs in its own project under `real-world-projects/` or another external solution, not inside the platform core.

## Documents

- [Architecture Foundation](docs/architecture-foundation.md)
- [Platform Product Boundary](docs/platform-product-boundary.md)
- [Pipeline Graph Foundation](docs/pipeline-graph-foundation.md)
- [Integration SDK Strategy](docs/integration-sdk-strategy.md)
- [SDK Quickstart](docs/sdk-quickstart.md)
- [Dataset-First MVP Roadmap](docs/dataset-first-mvp-roadmap.md)
- [Session Handoff](docs/session-handoff-2026-07-16.md)
