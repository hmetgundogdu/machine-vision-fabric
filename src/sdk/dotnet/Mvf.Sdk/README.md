# MachineVisionFabric.Sdk

The .NET SDK for authoring **MachineVisionFabric** integration modules — typed
graph nodes that run inside the local edge runtime.

MachineVisionFabric is an open-source, edge-first vision pipeline platform:
a strict, schema-validated typed graph with separate **data** and **control**
edges, built to keep running without a central server on panel/industrial PCs.

## What this package gives you

Base classes to implement the node contract without wiring the plumbing yourself:

- `FrameSourceModuleBase` — camera / stream / folder sources
- `FrameProcessorModuleBase` — frame → frame transforms
- `FrameClassifierModuleBase` — frame → typed classification
- `FrameSinkModuleBase` — sinks (dataset writers, PLC/control outputs)
- `ProductPresenceGateModuleBase` — control-flow gating
- Helpers: `FrameEnvelopeFactory`, `IntegrationModuleDescriptorBuilder`,
  `PackagePathResolver`, `BackgroundFrameSourceSession`

## Related SDKs

The same module protocol (stdio control plane + shared-memory data plane) has
Python (`mvf-sdk`) and C++ SDKs, so modules can be written in any of the three
languages. See the repository `protocol/README.md`.

## License

Apache-2.0
