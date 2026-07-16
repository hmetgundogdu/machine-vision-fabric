# MachineVisionFabric Integration SDK Strategy

## Goal

External developers should be able to build integrations in `.NET` against a stable public contract surface without editing platform runtime code.

## SDK Surface

The public SDK entry point today is:

- `MachineVisionFabric.Sdk`

That SDK wraps and exposes the lower-level platform surface from:

- `MachineVisionFabric.Contracts`
- `MachineVisionFabric.Core`

That surface defines:

- integration module contracts
- frame source contracts
- product gate contracts
- package/profile contracts
- dataset capture contracts

## Runtime Model

The platform runtime should:

- load integration assemblies dynamically
- keep vendor dependencies outside the platform core
- validate configuration through platform-owned schema
- remain deployable without any specific camera vendor dependency

## Repository Rule

- platform assemblies live under `src/`
- sample integrations live under `examples/integrations/`
- sample source simulators live under `examples/sources/`
- real customer adapters should follow the same boundary under `real-world-projects/` or another separate solution

## Current Technical Direction

Recommended implementation style:

1. contract-first extension model
2. isolated module loading with `AssemblyLoadContext`
3. schema export from `System.Text.Json`
4. runtime composition at the host or CLI edge

## Practical Consequence

If someone wants to integrate a camera SDK, they should create a separate `.NET` module that implements the platform contracts and is loaded by the runtime.

They should not patch `MachineVisionFabric.Runtime` or add vendor references into `src/`.
The default repository layout for this project is to place those modules under `real-world-projects/` with their own solution file.
