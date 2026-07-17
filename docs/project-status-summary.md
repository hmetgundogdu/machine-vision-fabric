# MachineVisionFabric Project Status Summary

Date: `2026-07-17`

## What This Project Is

MachineVisionFabric is being built as a headless, open-source machine-vision platform.

The core product is vendor-agnostic and is responsible for:

- loading runtime packages
- loading frame source modules
- loading product presence gate modules
- applying capture policy
- writing dataset output to disk

This means the platform itself does not own Cognex, PLC, or customer-specific code.

## Product Boundary

The repository is now split into three main areas:

- `src/` = platform product
- `examples/` = sample integrations and sample packages
- `real-world-projects/` = project-specific integrations and real station scenarios

This keeps the platform generic while still allowing real deployments to be built inside the same repository under a separate solution boundary.

## How The Current Runtime Works

Today the project is not a visual node editor yet.
It runs as a headless runtime with package-driven composition.

The execution flow is:

1. CLI opens a package
2. `manifest.json` and `profile.json` are loaded
3. a frame source module is resolved
4. a product presence gate module is resolved
5. frames are streamed into the runtime
6. capture policy decides what to persist
7. dataset output is written to:
   - `images/`
   - `metadata/`
   - `session.json`

## Current Architectural Model

Conceptually, the system is already aligned with a future node graph:

- camera = source node
- PLC or product-present signal = control node
- AI or processing step = processing node
- dataset writer, stream exporter, or downstream output = sink node

In the current headless MVP, those ideas map to runtime contracts:

- source node -> `IFrameSourceSession`
- control node -> `IProductPresenceGate`
- output behavior -> `DatasetCollector` and capture policy

Important clarification:

- the current runtime is still a fixed composition runtime
- it is not yet a full pipeline graph executor
- a real graph representation now starts from typed node, port, and edge contracts

## New Graph Foundation

The repository now includes the first pipeline graph contract layer:

- `PipelineDefinition`
- `PipelineNodeDefinition`
- `PipelinePortDefinition`
- `PipelineEdgeDefinition`
- `IPipelineDefinitionValidator`

This establishes the new architectural split:

- engine-owned embedded primitives such as `if`, `switch`, `fork`, `join`, and `loop`
- SDK-based external work nodes such as camera, PLC, inference, stream, and storage modules

The repository also now has a typed inspection direction:

- typed capability metadata on integration descriptors
- typed input/output port metadata for modules
- a package/runtime inspection service for resolved pipeline inspection

This closes an important future frontend risk:
the graph builder can inspect modules and runtime-resolved pipelines without relying on ad hoc string parsing.

This is the intended long-term direction for the platform.
The current headless dataset runtime remains active while graph execution is introduced incrementally.

## SDK and Integration Strategy

Real integrations are implemented as external modules using `MachineVisionFabric.Sdk`.

The important rule is:

- platform stays in `src/`
- real adapters are implemented as separate modules
- those modules are loaded dynamically by the runtime

This means Cognex was implemented as an independent source module, not as hardcoded platform logic.

## What Was Implemented For Cognex

A real Cognex integration was added under:

- `real-world-projects/integrations/MachineVisionFabric.Integrations.CognexCamera`

Important files:

- `CognexCameraIntegrationModule.cs`
- `CognexCameraSession.cs`
- `CognexCameraOptions.cs`

This integration uses:

- Cognex HMI WebSocket session
- HTTP image fetch
- resident source session model

No Cognex SDK DLL dependency was added to the platform core for this path.

## Real Camera Validation

The local machine was able to reach a live Cognex HMI endpoint:

- `http://10.159.131.19:8087/`

That endpoint was validated as Cognex HMI and then used by the real-world Cognex source module.

The runtime successfully captured real dataset frames from the camera.
The current passive HMI path now uses:

- `ready` heartbeat in passive mode
- OpenCV dark-frame filtering
- runtime-safe managed/native dependency loading for the external processor module

## Temporary Gate For Pipeline Testing

PLC integration is not ready yet.

To keep pipeline validation moving, a temporary delayed product gate was added.

Behavior:

- gate returns `false` for 10 seconds
- then returns `true`

This allows the system to mimic:

- product arrives later
- capture starts after gate opens

without requiring the real PLC implementation yet.

## Ready Packages

Current real-world Cognex packages:

- `dataset-capture-cognex-hmi-passive`
- `dataset-capture-cognex-hmi-trigger-window`
- `dataset-capture-cognex-delay-gate`

The most practical current test package is:

- `real-world-projects/packages/dataset-capture-cognex-delay-gate`

In that package, camera IP can be changed directly from:

- `profile.json`

## Release Output

A portable release flow was prepared for the temporary delayed-gate Cognex test scenario.

Local release artifacts are generated under:

- `artifacts/releases/mvf-cognex-delay-gate-20260716`
- `artifacts/releases/mvf-cognex-delay-gate-20260716.zip`

The release includes:

- published CLI runtime
- `examples/`
- `real-world-projects/`
- run script
- release README

The local run script is:

- `run-cognex-delay-gate.ps1`

## Current Project State

At this point the project already has:

- a working headless platform core
- a dynamic SDK-based integration model
- real-world project separation inside the same repository
- real Cognex HMI frame capture
- passive Cognex auto-trigger dataset capture
- OpenCV dark-frame processor integration
- dataset persistence
- temporary delayed gate behavior for no-PLC testing
- portable release packaging script
- first typed pipeline graph contracts and validation

## What Is Still Missing

The main missing production-side piece is the real PLC gate implementation for the station.

After that, the flow becomes:

- real product-present signal
- real Cognex source
- real dataset capture or downstream processing

The future UI / graph editor is still intentionally postponed.

The project direction remains:

- platform/core first
- real integrations via SDK
- headless-first
- UI later
