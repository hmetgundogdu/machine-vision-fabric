# MachineVisionFabric Session Handoff

Date: `2026-07-16`

## Summary

The project is currently:

- `headless-first`
- `dataset-first`
- `platform/core first, UI later`

The repository boundary is now explicit:

- `src/` contains the platform product
- `examples/` contains non-product sample integrations, simulators, tools, and packages
- `real-world-projects/` contains project-specific integrations and packages under its own solution boundary

## Verified State

As of `2026-07-16`, the runtime is verified to:

- load package manifests and profiles
- resolve source modules and gate modules through `.NET` contracts
- collect dataset sessions with per-frame metadata
- stream frames through `IFrameSourceSession`
- write captured data from both file-backed and memory-backed frame envelopes

Validated commands:

1. `dotnet build MachineVisionFabric.slnx -v minimal`
2. `dotnet test MachineVisionFabric.slnx -v minimal`
3. `dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run`
4. `dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-no-product`
5. `dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-tcp-plc`
6. `dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-s7-gateway`
7. `dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --package examples\packages\dataset-capture-conveyor-sim --dataset-root artifacts\datasets-live --session-prefix live-test`

## Important Paths

- [Platform root](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\src)
- [Examples root](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples)
- [Real-world projects](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\real-world-projects)
- [Starter package](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\packages\dataset-capture-starter)
- [Conveyor package](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\packages\dataset-capture-conveyor-sim)

## Next Step

The next technical step should be:

1. build the first real camera SDK integration module under `real-world-projects/`
2. add PLC-trigger-aware frame buffering policy
3. continue treating `examples/` as sample code, not product code
