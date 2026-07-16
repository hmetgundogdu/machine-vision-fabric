# MachineVisionFabric

MachineVisionFabric is an open-source edge vision platform for headless dataset collection, device integration, PLC-gated capture, and future machine-vision execution on edge devices.

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
|-- examples/             non-product example code
|   |-- integrations/     sample .NET modules
|   |-- sources/          simulator source implementations
|   |-- packages/         sample runtime packages
|   `-- tools/            helper simulators
|-- real-world-projects/  project-specific integrations and scenarios
|   `-- MachineVisionFabric.RealWorld.slnx
|-- tools/                platform-owned tooling
|   `-- MachineVisionFabric.SchemaExporter/
|-- tests/
`-- docs/
```

`src/` is the platform.
`examples/` exists to demonstrate the SDK surface and to support local development.
`real-world-projects/` is where project-specific camera and scenario work should begin.
It is intentionally kept in the same repository, but under its own folder and solution boundary.

## Current Direction

The project is intentionally `headless-first` and `dataset-first`.

The first MVP goal is:

- reliable runtime bootstrap
- package and profile loading
- dataset session creation
- source and gate resolution through `.NET` modules
- simulator-driven capture without hardware

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

Validated example packages:

- `examples/packages/dataset-capture-starter`
- `examples/packages/dataset-capture-no-product`
- `examples/packages/dataset-capture-s7-gateway`
- `examples/packages/dataset-capture-tcp-plc`
- `examples/packages/dataset-capture-conveyor-sim`
- `examples/packages/dataset-capture-trigger-window`
- `examples/packages/dataset-capture-resident-camera-stub`

## Run

Build and test:

```powershell
dotnet build MachineVisionFabric.slnx -v minimal
dotnet test MachineVisionFabric.slnx -v minimal
```

List packages:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll packages
```

List example modules:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll modules
```

Run the default example package:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run
```

Run the product-absent example:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-no-product
```

Inspect an example package:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll inspect-package --package examples\packages\dataset-capture-tcp-plc
```

Run the TCP signal simulator:

```powershell
dotnet examples\tools\MachineVisionFabric.TcpSignalSimulator\bin\Debug\net10.0\MachineVisionFabric.TcpSignalSimulator.dll --port 15020 --value 1
```

Run the conveyor dataset example:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-conveyor-sim --dataset-root artifacts\datasets-live --session-prefix live-test
```

Run the trigger-window example:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-trigger-window --dataset-root artifacts\datasets-trigger --session-prefix trigger-window
```

Run the resident camera stub example:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-resident-camera-stub --dataset-root artifacts\datasets-resident --session-prefix resident-camera
```

## Integrator Direction

External developers should:

- build `.NET` integration modules against `MachineVisionFabric.Sdk`
- load them through the platform runtime
- validate config with exported JSON schema
- keep vendor SDK code outside `src/`

That means a real camera adapter belongs in its own project under `real-world-projects/` or another external solution, not inside the platform core.

## Documents

- [Platform Product Boundary](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\platform-product-boundary.md)
- [Integration SDK Strategy](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\integration-sdk-strategy.md)
- [SDK Quickstart](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\sdk-quickstart.md)
- [Dataset-First MVP Roadmap](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\dataset-first-mvp-roadmap.md)
- [Session Handoff](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\session-handoff-2026-07-16.md)
- [Examples README](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\README.md)
