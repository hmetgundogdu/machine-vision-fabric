# CLAUDE.md

## Project Context

`DynoVisionPipeline` is an open-source edge vision pipeline platform.

Primary operating assumptions:

- runs inside company networks
- targets panel PCs, industrial PCs, and NUC-class edge devices
- starts `Windows-first`
- must keep running without a central server
- central systems are optional for observability and fleet visibility

## Product Direction

This is not a single-camera desktop app.

It is intended to become a:

- graph-based pipeline engine
- local edge runtime
- web-based pipeline studio
- optional telemetry publisher to a central observer

## Core Architectural Rules

### 1. Strict Typed Graph

Pipelines must be strict, directional, and schema-validated.

Preferred mental model:

```text
NODE(typed output) -> NODE(typed input) -> NODE(typed output)
```

Do not introduce loose runtime object passing as the default behavior.

Required ideas:

- typed input ports
- typed output ports
- explicit config schema
- runtime validation in addition to UI validation

### 2. Separate Data Flow and Control Flow

There are two edge types:

- `data edge`
- `control edge`

Examples:

- frame or tensor transfer is data flow
- PLC presence decision or branch selection is control flow

Do not collapse them into one generic link model.

### 3. Edge-First Execution

The edge runtime owns:

- camera or stream access
- PLC/control integration
- AI inference
- pipeline execution
- local persistence
- optional local streaming

The central system does not own execution.

The central system may observe:

- logs
- health
- inventory
- optional pipeline telemetry

## Technology Defaults

- engine: `C# / .NET 10 LTS`
- studio UI: `React + TypeScript + React Flow`
- desktop shell if ever needed: `Tauri`
- inference: `ONNX Runtime`
- media bridge: `MediaMTX` first, `GStreamer` if necessary later
- telemetry first choice: `WebSocket`

## Lifecycle Defaults

Cold start cost matters.

Use these defaults unless there is a strong reason not to:

- `camera/source`: resident
- `plc/control`: resident
- `ai-model`: resident and preloaded
- `short helper process`: on-demand
- `heavy external worker`: resident by default, override allowed

Do not treat lifecycle as an implementation detail; it is part of the node contract.

## Simulator-First Principle

Real vendor cameras are not required for the first milestone.

Prefer building simulator nodes first:

- folder sequence camera
- loop image camera
- side-by-side multi-frame simulator
- scenario-based simulator

This project should be testable without hardware.

## Packaging Direction

Pipelines should support `JSON + folder package` import/export rather than a single file only.

Expected assets include:

- models
- scripts
- helper executables
- configs

## Telemetry Rule

Telemetry must be:

- optional
- non-blocking
- best-effort

The execution hot path must never wait on telemetry publishing.

## Contribution Guidance

When making changes:

1. Read `docs/architecture-foundation.md`
2. Preserve strict typing and edge/control separation
3. Avoid baking vendor-specific assumptions into the core
4. Favor simulator-friendly design
5. Update docs when a real architectural decision changes
