# MachineVisionFabric Pipeline Graph Foundation

## Purpose

MachineVisionFabric now has two different runtime layers:

1. the current dataset-first composition runtime
2. the future typed pipeline graph runtime

They are related, but they are not the same thing.

The current runtime can already compose:

- one frame source
- one product presence gate
- one frame processor
- one dataset storage flow

That is useful and real, but it is not yet a full pipeline graph representation.

## Current State

Today the package model is still a runtime composition profile.

It answers:

- which source module should run
- which gate should run
- which processor should run
- where output should be persisted

It does not yet model:

- arbitrary node graphs
- port-to-port wiring
- branching
- fan-out
- fan-in
- embedded control-flow primitives

## Target Representation

The long-term pipeline representation must be a typed directed graph.

Each pipeline definition should contain:

- nodes
- edges
- typed input ports
- typed output ports
- data edges
- control edges

The basic contract is:

`NODE(typed output) -> NODE(typed input)`

Invalid links must be rejected before execution.

## Node Ownership Model

There are currently three kinds of nodes.

### Embedded Primitive Nodes

These are owned by the platform engine and live in `src/`.

Examples:

- `if`
- `switch`
- `fork`
- `join`
- `merge`
- `loop`
- `retry`
- `delay`
- `buffer`
- `throttle`
- `sample`
- `drop`

These are not integration work. They are pipeline language primitives.

They should stay embedded because they define:

- execution semantics
- scheduling behavior
- branching behavior
- queueing behavior
- retry policy

### Runtime-Builtin Nodes

These are also platform-owned and live in `src/`, but they are not flow-control primitives.

Examples:

- built-in simulator source
- built-in dataset writer
- temporary compatibility bridge nodes generated from dataset-first package composition

These exist because the platform is migrating from fixed runtime composition to a real graph executor.
They should remain platform-owned until they are either promoted into stable engine nodes or replaced by explicit graph execution services.

### External Work Nodes

These are implemented through the SDK and stay outside the platform core.

Examples:

- camera source
- PLC gate
- AI inference
- image preprocessing
- file output
- stream publisher
- customer-specific process launcher

These should stay external because they carry:

- vendor SDK dependency
- station-specific assumptions
- customer logic

## First Contract Layer

The repository now contains the first graph contracts under:

- `MachineVisionFabric.Contracts.Pipelines`

These contracts define:

- `PipelineDefinition`
- `PipelineNodeDefinition`
- `PipelinePortDefinition`
- `PipelineEdgeDefinition`
- `PipelinePortReference`
- `PipelineValidationResult`

The first validator is registered in runtime as:

- `IPipelineDefinitionValidator`

Current validation scope:

- duplicate node id check
- duplicate edge id check
- embedded primitive vs integration module vs runtime-builtin rules
- input/output port existence check
- edge channel compatibility
- edge data type compatibility

## Typed Inspection Surface

There must be a single typed inspection point for both:

- available SDK modules
- the resolved runtime pipeline for a package

This is now a first-class architectural requirement.

Why it matters:

- frontend graph builders must inspect reusable modules safely
- package authors must see exactly what the runtime resolved
- module catalog reuse depends on stable typed metadata
- node wiring UI must never rely on loose string guessing

Required inspection outputs:

- module id
- capability kind
- config schema type
- typed input ports
- typed output ports
- resolved pipeline nodes
- resolved pipeline edges
- validation issues

The key rule is:

- module authors write typed contracts once
- frontend and runtime both consume the same typed inspection surface

## Current Implementation Direction

The repository now includes:

- typed port metadata on integration capability descriptors
- a package/runtime inspection service contract
- a runtime inspection service implementation
- CLI inspection output for the resolved pipeline and available modules

This is the intended future API surface for studio and catalog features.

## What This Means Practically

Short term:

- dataset-first packages keep running as they are
- current Cognex and gate scenarios remain valid
- no UI migration is required yet

Next step:

- introduce package-level optional `pipeline.json`
- map current source/gate/processor composition into graph nodes
- add embedded primitive execution
- gradually move runtime execution from fixed composition to graph execution

That package-level `pipeline.json` support now exists conceptually in the runtime path:

- if a package declares `pipelineDefinition`, the runtime can load that graph file
- if it does not, the runtime generates a synthetic compatibility graph from the current package/profile model

## Design Rule Going Forward

If something changes execution flow, it is probably an embedded primitive.

If something talks to a device, model, SDK, file system, process, or protocol, it is probably an external work node.
