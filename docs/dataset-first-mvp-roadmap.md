# MachineVisionFabric Dataset-First MVP Roadmap

## Goal

The first MVP should not try to solve the full platform.

Its job is simpler:

- bootstrap a headless runtime
- collect datasets reliably
- prepare package, simulator, PLC gate, and storage foundations

The first business outcome is:

- when a product arrives, the system can decide whether to collect frames
- those frames can be saved in a structured dataset layout
- the collection flow can later evolve into inference, streaming, and advanced pipelines

## Why Dataset First

This direction is better because:

- real model quality depends on real data more than early UI
- storage, naming, capture triggers, and package structure are core platform concerns
- simulator and PLC control paths can be validated before vendor camera work
- it forces the runtime contracts to become practical instead of decorative

## MVP Scope

### Included

- headless host
- runtime bootstrap
- dataset collection profile
- simulator source support
- PLC-driven control node design target
- folder-based package import structure
- local storage preparation
- optional future telemetry hooks, but not a full telemetry product

### Excluded for now

- UI / Studio
- real vendor camera SDK integration
- ONNX inference execution
- central control panel
- live stream serving
- advanced branching editor

## Product Decision

For this MVP, dataset collection should be treated as a first-class capability of the system, not as a side effect.

Recommended direction:

- the runtime owns dataset capture capability
- platform profiles define capture behavior and source settings
- graph definitions are now a foundation contract layer, but not yet the main execution surface

## Current Delivery Status

Current verified state on `2026-07-16`:

- headless host is implemented
- package `manifest.json` loading is implemented
- entry `profile.json` loading is implemented
- simulator frame enumeration is implemented
- dataset image persistence is implemented
- per-frame metadata sidecars are implemented
- session-level `session.json` output is implemented
- external `.NET` product presence gate loading is implemented
- product-absent capture skipping is implemented

Current foundation state on `2026-07-17`:

- typed pipeline graph contracts are added
- embedded primitive vs external integration node ownership is defined
- first graph validator is added
- graph execution is not yet the active runtime path

## Phase Breakdown

### Phase 1: Headless Bootstrap

- solution and project structure
- host process
- runtime startup
- config loading
- package path validation
- storage root validation

Status:
Completed for the current simulator-driven samples.

### Phase 2: Dataset Collection Foundation

- dataset session concept
- capture policies
- output folder conventions
- file naming conventions
- metadata sidecars

Status:
Completed for the current simulator-driven samples.

### Phase 3: Simulator-Driven Collection

- folder sequence simulator
- multi-simulator support
- frame interval and looping
- scenario-driven collection runs

Status:
Completed for the current simulator-driven samples.

### Phase 4: PLC Gate Preparation

- define control node contract
- declare station presence schema
- define capture gating behavior
- keep real S7 implementation for a later step

Status:
The external gate module pattern is implemented through the simulated gate sample. Real PLC integration is still pending.

### Phase 5: Inference-Ready Extension

- model asset placement in package
- warmup and resident policy
- future ONNX node contract

Status:
Not started.

### Phase 6: Typed Pipeline Graph

- define node, port, and edge contracts
- define embedded primitive categories
- validate type-safe graph links before execution
- keep dataset-first runtime alive while graph execution is introduced incrementally

Status:
Started. Contracts and validator are in place. Execution migration is still pending.

## Recommended Initial Dataset Layout

```text
datasets/
`-- session-YYYYMMDD-HHMMSS-fff/
    |-- images/
    |-- metadata/
    |-- rejected/
    `-- session.json
```

## Recommended First Deliverable

The first working deliverable should be:

- a headless service that starts
- reads a dataset capture package
- validates directories
- prepares a dataset session
- loads simulator source configuration
- saves frames and metadata into the dataset session

This deliverable is now complete for simulator-driven samples.

## Next Steps After This MVP

1. Promote camera/source integration to the same external `.NET` module model used by the gate.
2. Add a formal integration package manifest and discovery contract.
3. Add real PLC implementation modules after the simulated gate example.
4. Add package-driven capture triggers beyond simple presence gating.
5. Add a small headless CLI or service API for runtime selection and control.
