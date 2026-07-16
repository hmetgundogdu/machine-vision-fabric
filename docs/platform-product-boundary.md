# MachineVisionFabric Platform Boundary

## Core Rule

There are two different layers:

1. `MachineVisionFabric Platform`
2. `Project-specific extensions and future pipeline UX`

They must stay separate.

## What The Platform Owns

The platform owns:

- public contracts
- runtime orchestration
- package and profile loading
- plugin/module loading
- dataset session storage
- operational CLI and host surfaces

In this repository, that means `src/`.

## What The Platform Does Not Own

The platform does not own:

- vendor camera SDK adapters
- customer PLC adapters
- customer inference wrappers
- customer process launchers
- station-specific business logic

In this repository, those belong under `examples/` only as samples.
For actual project work in this repository, they should live under `real-world-projects/` in a separate solution boundary.

## Repository Interpretation

- `src/` = platform product
- `examples/` = sample implementations of the SDK surface
- `real-world-projects/` = project-specific integrations and packages in a separate solution
- `tools/` = platform-owned tooling

If a new component requires a vendor DLL or customer-specific assumptions, it should not go into `src/`.

## Current Decision

For MachineVisionFabric:

- the runtime stays generic
- integrations happen through public `.NET` contracts
- example integrations can ship in-repo for development
- real project integrations should remain external to the platform core, even if they stay in the same repository
