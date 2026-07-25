# MachineVisionFabric

[![CI](https://github.com/hmetgundogdu/machine-vision-fabric/actions/workflows/ci.yml/badge.svg)](https://github.com/hmetgundogdu/machine-vision-fabric/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hmetgundogdu/machine-vision-fabric?sort=semver)](https://github.com/hmetgundogdu/machine-vision-fabric/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**MachineVisionFabric (MVF)** is an open-source, **edge-first** vision pipeline platform.
It runs a strict, schema-validated **typed graph** of nodes — cameras, transforms, AI
inference, PLC/control, storage — directly on panel PCs, industrial PCs, and NUC-class edge
devices. It keeps running without a central server; a central system is optional, and only
for observability.

<p align="center">
  <img src="docs/assets/cli-demo.svg" alt="mvf CLI live graph dashboard running inspection-demo" width="860">
</p>

Above is the CLI's **live graph dashboard** running `packages/inspection-demo` — a 13-node graph
(folder-source simulator → fork → Python brightness/counter + invert workers → switch → routed
dataset sinks) executing end-to-end with **no hardware**. Node boxes are colour-coded by
category, show their live config and per-node stats, and the executing node is highlighted.

## Why MVF

- **Strict typed graph** — every edge is typed and schema-validated at runtime, not just in a UI.
  `NODE(typed output) → NODE(typed input)`.
- **Data flow ≠ control flow** — frame/tensor transfer (**data edges**) and branch/gate
  decisions (**control edges**) are separate first-class link types, never collapsed.
- **Edge-first execution** — the edge runtime owns camera/stream access, PLC integration,
  inference, execution, and local persistence. The central system only *observes*.
- **Polyglot modules, one protocol** — modules run out-of-process over a small stdio control
  plane + a zero-copy **shared-memory data plane** (typed payloads, no base64). Author them in
  **.NET, Python, or C++** interchangeably.
- **Simulator-first** — testable without vendor cameras; folder/loop/scenario simulators ship in-box.

See [`docs/architecture-foundation.md`](docs/architecture-foundation.md) and
[`docs/roadmap.md`](docs/roadmap.md) for the full design.

## Repository layout

```text
machine-vision-fabric/
├── src/
│   ├── core/            Mvf.Graph (typed graph + validation), Mvf.Abstractions (contracts)
│   ├── engine/          Mvf.Engine (pipelined scheduler, checkpoint/restore, backpressure)
│   ├── hosting/         Mvf.Hosting.Worker (out-of-process module host)
│   ├── transports/      Mvf.Transport.SharedMemory (zero-copy data plane)
│   ├── sdk/
│   │   ├── dotnet/      Mvf.Sdk         → NuGet:  MachineVisionFabric.Sdk
│   │   ├── python/      mvf_sdk         → wheel:  mvf-sdk
│   │   └── cpp/         mvf/sdk.hpp     → lib:    libmvf_sdk.{so,dylib} / mvf_sdk.dll
│   └── cli/             Mvf.Cli (headless host + live TUI dashboard)
├── modules/             integration modules (.NET + Python) discovered at runtime
├── packages/            runnable pipeline packages (pipeline.json)
├── tools/               Mvf.SchemaExporter
├── tests/               engine test suite
└── protocol/            the language-agnostic module wire protocol
```

## Install

### Download the CLI (no .NET required)

Grab a single self-contained executable for your OS from the
[latest release](https://github.com/hmetgundogdu/machine-vision-fabric/releases/latest):

| OS | Asset |
|---|---|
| Linux | `mvf-cli-linux-x64` |
| macOS (Apple Silicon) | `mvf-cli-osx-arm64` |
| Windows | `mvf-cli-win-x64.exe` |

```bash
chmod +x mvf-cli-linux-x64
./mvf-cli-linux-x64 packages
```

### Build from source

Requires the .NET SDK pinned in `global.json` (.NET 10).

```bash
dotnet build Mvf.slnx -c Release
dotnet run --project src/cli/Mvf.Cli -- packages
```

## Quickstart

```bash
# List runnable pipeline packages and discovered modules
dotnet run --project src/cli/Mvf.Cli -- packages
dotnet run --project src/cli/Mvf.Cli -- modules

# Validate a pipeline graph
dotnet run --project src/cli/Mvf.Cli -- validate-pipeline --path packages/inspection-demo/pipeline.json

# Run the inspection demo — live TUI dashboard
dotnet run --project src/cli/Mvf.Cli -- execute-graph --package packages/inspection-demo

# ...or headless (as in the screenshot): plain output, stop after N cycles
dotnet run --project src/cli/Mvf.Cli -- execute-graph --package packages/inspection-demo --no-tui --max-cycles 3
```

### Demo packages (hardware-free)

| Package | Shows |
|---|---|
| `inspection-demo` | 13-node inspection graph, polyglot workers, routed dataset sinks |
| `loop-demo` | graph iteration authority (`loop` primitive, whole-graph pause) |
| `value-demo` | typed value / select primitives |
| `multilang-demo` | .NET + Python nodes in one graph |
| `py-brightness-demo`, `py-invert-demo` | single Python classifier / transformer |

## SDKs — author your own modules

All three SDKs speak the **same** protocol (stdio control plane + shared-memory data plane,
see [`protocol/README.md`](protocol/README.md)), so a module is interchangeable across languages.

**Python** — `pip install mvf-sdk`

```python
from mvf_sdk import run_processor, blob

def transform(payload, meta):
    return blob(bytes(255 - b for b in payload.memory))   # invert every byte

run_processor("py.invert-transformer", transform)
```

**C++** — link `libmvf_sdk` (see [`src/sdk/cpp/README.md`](src/sdk/cpp/README.md))

```cpp
#include "mvf/sdk.hpp"
using namespace mvf;
int main() {
    return run_processor("cpp.invert-transformer",
        [](const Payload& in, const json&) -> std::optional<Output> {
            std::string out(in.size, '\0');
            for (size_t i = 0; i < in.size; ++i) out[i] = static_cast<char>(255 - in.data[i]);
            return blob(std::move(out));
        });
}
```

**.NET** — reference `MachineVisionFabric.Sdk` and derive from a module base
(`FrameProcessorModuleBase`, `FrameClassifierModuleBase`, `FrameSourceModuleBase`,
`FrameSinkModuleBase`). A processor returns an accept/reject decision:

```csharp
public sealed class BrightnessGateModule : FrameProcessorModuleBase<BrightnessGateOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateProcessor<BrightnessGateOptions>(
            "mvf.example-brightness-gate", "Brightness Gate", "1.0.0", "brightness-gate",
            "Accepts a frame only when its mean byte value meets a threshold.");

    protected override IFrameProcessor CreateProcessor(BrightnessGateOptions o) => new Gate(o);
    // ...IFrameProcessor.EvaluateAsync returns a FrameProcessorDecision (accept/reject)
}
```

Full runnable example: [`modules/dotnet-brightness-gate/`](modules/dotnet-brightness-gate/) ·
authoring guide: [`docs/sdk-quickstart.md`](docs/sdk-quickstart.md).

## Releases & CI

- **CI** (`.github/workflows/ci.yml`) runs on every push/PR: builds + tests the .NET solution
  (with Python for the module-spawning tests), builds the Python wheel, and compiles the C++
  SDK on Linux/macOS/Windows.
- **Release** (`.github/workflows/release.yml`) runs on a `v*` tag and publishes, all versioned
  from the tag:
  - `.nupkg` — `MachineVisionFabric.Sdk`
  - `.whl` + sdist — `mvf-sdk`
  - `mvf-sdk-cpp-<ver>-{linux-x64,osx-arm64,win-x64}.zip` — C++ shared library + header
  - `mvf-cli-{linux-x64,osx-arm64,win-x64}` — self-contained single-file CLI

Cut a release by tagging: `git tag v0.1.2 && git push origin v0.1.2`.

## Documentation

- [Architecture Foundation](docs/architecture-foundation.md)
- [Pipeline Graph Foundation](docs/pipeline-graph-foundation.md)
- [Integration SDK Strategy](docs/integration-sdk-strategy.md) · [SDK Quickstart](docs/sdk-quickstart.md)
- [Roadmap](docs/roadmap.md)

## License

[Apache-2.0](LICENSE)
