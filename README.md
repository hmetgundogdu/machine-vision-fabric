# MachineVisionFabric

[![CI](https://github.com/hmetgundogdu/machine-vision-fabric/actions/workflows/ci.yml/badge.svg)](https://github.com/hmetgundogdu/machine-vision-fabric/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hmetgundogdu/machine-vision-fabric?sort=semver)](https://github.com/hmetgundogdu/machine-vision-fabric/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**MachineVisionFabric (MVF)** is an open-source edge runtime for **typed vision pipelines**.

- **Graph:** a strict, directional graph of nodes, written in **Python, C++ or C#** and mixed freely
  in one pipeline.
- **Data:** typed payloads (images, tensors, blobs, JSON) move between nodes **zero-copy** through
  a shared-memory arena.
- **Edges:** data flow and control flow are separate, typed edge kinds.
- **Edge-first:** the runtime runs self-contained on panel PCs, industrial PCs and NUC-class
  devices. A central system is optional and only observes.

The runtime is **`mvf`**, a single native executable written in Rust, with no .NET, JVM or Docker
dependency.

## Why MVF

- **Polyglot, one protocol.** Python, C++ and .NET nodes are supervised worker processes that
  speak the same stdio control protocol, with payloads in the shared-memory arena ([worker
  protocol v2](protocol/worker-v2.md)).
- **Strict typed graph.** Ports, payload types and config schemas are validated when the
  pipeline loads, and again at run time: `NODE(typed output) → NODE(typed input)`.
- **Data flow ≠ control flow.** Frames and tensors move on data edges. Decisions (`if`,
  `switch`, gates) move on control edges.
- **Built for the line:**
  - serial or pipelined execution, with per-node parallelism;
  - failure policies (`fail`, `skip`, `restart`) and backpressure;
  - checkpoints and resume;
  - live node replacement with probation and rollback.
- **Fabrics for real deployments:** networking ([#23](https://github.com/hmetgundogdu/machine-vision-fabric/issues/23)),
  storage (volumes, pinned models; [#31](https://github.com/hmetgundogdu/machine-vision-fabric/issues/31)),
  display (windows and input from nodes; [#30](https://github.com/hmetgundogdu/machine-vision-fabric/issues/30)),
  and optional daemonless OCI containers on Linux.
- **Observable and manageable:**
  - an authenticated local management endpoint for status, events, commands and opt-in
    previews;
  - an optional mutual-TLS relay (`mvf-remote`);
  - signed package upload ([#33](https://github.com/hmetgundogdu/machine-vision-fabric/issues/33)).
- **Testable without hardware.** Simulator sources (folder sequence, synthetic camera) are built
  in.

Design: [`docs/architecture-foundation.md`](docs/architecture-foundation.md) and the runtime's
decision records in [`docs/rust-runtime/`](docs/rust-runtime/README.md).

## Repository layout

```text
machine-vision-fabric/
├── rust/                 the runtime: Cargo workspace (crates/), examples/, scripts/
│   └── crates/mvf-cli    → the `mvf` executable
├── src/sdk/
│   ├── python/           mvf_sdk                 → wheel: mvf-sdk
│   ├── cpp/              mvf/sdk.hpp             → libmvf_sdk.{so,dylib} / mvf_sdk.dll
│   └── dotnet/Mvf.Worker C# worker SDK           → NuGet: MachineVisionFabric.Worker
├── modules/              worker modules with node.yaml contracts (Python examples)
├── packages/             legacy-format pipeline packages (pipeline.json), kept as import/parity fixtures
├── clients/mvf-manage-py Python client for the management endpoint
├── protocol/             worker and management protocols
└── docs/                 design documents
```

## Install

### Download

Get `mvf-<version>-<os>.zip` from the
[latest release](https://github.com/hmetgundogdu/machine-vision-fabric/releases/latest). It is
available for Linux x64, macOS arm64 and Windows x64, and holds:

| | |
|---|---|
| `mvf` | validate, run, import and manage pipelines |
| `mvf-remote` | optional mutual-TLS relay to a runtime's management endpoint, and package signing |
| `mvf-display-host` | optional window host for nodes that present images (Display Fabric) |

Python modules need Python 3 and the SDK, `pip install mvf-sdk`. In a checkout, `mvf` finds
`src/sdk/python` by itself.

### Build from source

Needs the Rust toolchain pinned in `rust/rust-toolchain.toml`:

```bash
cd rust && cargo build --release -p mvf-cli     # → rust/target/release/mvf
```

## Quickstart

```bash
mvf host                                                     # builtins and backends on this machine
mvf validate rust/examples/inspection-demo/pipeline.yaml     # YAML, contracts, typed edges, placement
mvf run rust/examples/loop-demo/pipeline.yaml --max-cycles 50
mvf run rust/examples/inspection-demo/pipeline.yaml --max-cycles 100 --mode pipelined
```

Observe a running pipeline:

```bash
mvf run rust/examples/inspection-demo/pipeline.yaml --manage --preview cam
mvf manage watch            # live node stats and events, from another terminal
mvf manage preview --node cam --count 3 --out shots
```

### Example pipelines (hardware-free)

| `rust/examples/…` | Shows |
|---|---|
| `inspection-demo` | 13 nodes: fork, switch, value, loop, three Python workers, routed dataset sinks |
| `loop-demo` | the `loop` primitive (iteration owner), live value and module binding |
| `value-demo` | typed `value` / `select` primitives with bindings |
| `gated` | control edges gating a branch |
| `live-update` | live node replacement and rollback from an update plan |
| `python-worker` | a single Python worker node |
| `display-demo` | a node that opens windows and reacts to clicks |

## Write a node (Python · C++ · C#)

A node is a small program that receives a typed payload and returns a result:

- **processor:** a new payload out;
- **classifier:** a label for `if`/`switch` routing;
- **analyzer:** an optional derived frame, a decision and structured metadata;
- **source** or **sink**.

### 1 · Write it

```python
# Python: pip install mvf-sdk
from mvf_sdk import run_processor, blob

def transform(payload, meta):
    return blob(bytes(255 - b for b in payload.memory))   # read in place from shared memory

run_processor("py.invert", transform)
```

```cpp
// C++: link libmvf_sdk
#include "mvf/sdk.hpp"
using namespace mvf;
int main() {
    return run_processor("cpp.invert",
        [](const Payload& in, const json&) -> std::optional<Output> {
            std::string out(in.size, '\0');
            for (size_t i = 0; i < in.size; ++i) out[i] = static_cast<char>(255 - in.data[i]);
            return blob(std::move(out));
        });
}
```

```csharp
// C#: NuGet MachineVisionFabric.Worker (a worker executable, no in-process plugin)
using Mvf.Worker;

return WorkerModule.RunProcessor("dotnet.invert", (payload, _) =>
{
    var input = payload.Data;
    var inverted = new byte[input.Length];
    for (var i = 0; i < input.Length; i++) inverted[i] = (byte)(255 - input[i]);
    return Output.Blob(inverted);
});
```

### 2 · Declare it: `node.yaml`

```yaml
apiVersion: mvf.dev/v1alpha1
kind: NodeContract
metadata: { id: py.invert, version: 1.0.0, description: Inverts every byte. }
spec:
  ports:
    inputs:  [{ name: frame, channel: data, type: frame }]
    outputs: [{ name: frame, channel: data, type: frame }]
  artifacts:
    - { backend: worker, runtime: python, entry: invert.py, protocol: 1 }
  lifecycle: { replacement: stateless }
```

Put it in a directory under `modules/`; `mvf` finds it by `metadata.id`. The full contract
(config schema, lifecycle, permissions, storage, display) is in
[`docs/rust-runtime/node-contract.md`](docs/rust-runtime/node-contract.md).

### 3 · Wire it: `pipeline.yaml`

```yaml
apiVersion: mvf.dev/v1alpha1
kind: Pipeline
metadata: { name: my-pipeline }
spec:
  nodes:
    - { id: cam, builtin: folder-source, config: { sourceFolder: frames } }
    - { id: invert, module: py.invert }
    - { id: save, builtin: dataset-writer, config: { outputRoot: out } }
  edges:
    data:
      - { from: cam.frame, to: invert.frame }
      - { from: invert.frame, to: save.frame }
```

The data graph is a strict DAG, so this runs one pass over the folder. To keep running, add a
`loop` node (`primitive: loop`, `config: { mode: forever }`) and close the tail into it under
`edges.loop` (`{ from: save, to: cycle }`). See
[`docs/rust-runtime/graph-primitives.md`](docs/rust-runtime/graph-primitives.md).

### 4 · Run it

```bash
mvf run my-pipeline/pipeline.yaml
```

## Coming from the legacy .NET runtime

The original .NET engine, CLI, in-process SDK (`MachineVisionFabric.Sdk`), in-process modules
and egress are **archived** ([#34](https://github.com/hmetgundogdu/machine-vision-fabric/issues/34)):

- **Code:** branch [`archive/dotnet-legacy`](https://github.com/hmetgundogdu/machine-vision-fabric/tree/archive/dotnet-legacy),
  tag `dotnet-legacy-final`.
- **Last release that ships it:** [v1.1.0](https://github.com/hmetgundogdu/machine-vision-fabric/releases/tag/v1.1.0)
  (`mvf-cli-<os>`, and the `MachineVisionFabric.Sdk` 1.1.0 packages).

To move a legacy package:

```bash
mvf import packages/my-line        # pipeline.json → pipeline.yaml next to it, then validated
```

- **Converted:** ids, builtins, edge kinds and the .NET failure behaviour.
- **Refused:** anything it cannot carry over.
- **Named:** every .NET in-process module that needs a `Mvf.Worker` port.

The steps, the command mapping and the rollback are in the
[migration guide](docs/rust-runtime/migration-guide.md).

## Releases & CI

- **CI** (`.github/workflows/ci.yml`) runs on every push and PR:
  - the runtime on Linux, macOS and Windows: format, lint, tests with real Python, C++ and .NET
    workers, and the real-window display smoke test;
  - the .NET worker SDK, the Python wheel and the C++ SDK.
- **Release** (`.github/workflows/release.yml`) runs on a `v*` tag that matches
  `rust/Cargo.toml`, and publishes:
  - `mvf-<tag>-{linux-x64,osx-arm64,win-x64}.zip`;
  - `MachineVisionFabric.Worker` (`.nupkg`);
  - `mvf-sdk` (wheel + sdist);
  - `mvf-sdk-cpp-<tag>-<os>.zip`.

## Documentation

- [Runtime overview and decision records](docs/rust-runtime/README.md)
- [Node contract](docs/rust-runtime/node-contract.md) · [Graph primitives](docs/rust-runtime/graph-primitives.md)
- [Worker protocol v2](protocol/worker-v2.md) · [Management protocol](protocol/management-v1.md)
- [Migration from the legacy runtime](docs/rust-runtime/migration-guide.md) · [Retirement inventory](docs/rust-runtime/legacy-retirement.md)
- [Architecture foundation](docs/architecture-foundation.md) · [Roadmap](docs/roadmap.md)

## License

[Apache-2.0](LICENSE)
