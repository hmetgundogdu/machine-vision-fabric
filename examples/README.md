# Examples Boundary

Everything under `examples/` is intentionally outside the platform product boundary.

This folder contains:

- sample integration modules
- simulator-backed source implementations
- sample runtime packages
- sample typed pipeline graph definitions
- helper tools used for local validation

Recommended starting point for a real camera adapter:

- `integrations/MachineVisionFabric.Integrations.ResidentCameraStub`

Useful sample packages:

- `packages/dataset-capture-starter`
- `packages/dataset-capture-conveyor-sim`
- `packages/dataset-capture-trigger-window`
- `packages/dataset-capture-resident-camera-stub`

Useful sample pipeline graph:

- `pipelines/dataset-capture-typed-graph/pipeline.json`

Rules:

- do not treat `examples/` as platform core
- do not put vendor SDK dependencies into `src/`
- if a customer-specific adapter is needed, model it like an `examples/integrations` project and keep it separate from the platform

Current layout:

```text
examples/
|-- integrations/
|-- packages/
|-- pipelines/
|-- sources/
`-- tools/
```
