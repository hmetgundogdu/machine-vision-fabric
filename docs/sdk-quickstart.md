# SDK Quickstart

How to author an MVF integration module. All three SDKs speak the same protocol (stdio
control plane + shared-memory data plane, see [`protocol/README.md`](../protocol/README.md)),
so a module is interchangeable across languages. See
[integration-sdk-strategy.md](integration-sdk-strategy.md) for the why.

## Typed authoring rule

Modules stay **fully typed**: typed options/config, typed capability kind, typed input and
output ports, and metadata the platform can inspect. This is a hard requirement for future
graph-authoring UX. The SDK is for **work nodes** — not for engine-owned flow-control
primitives (`if`, `switch`, `fork`, `loop`), which live in the engine.

## Python — `pip install mvf-sdk`

A processor returns a new typed payload (or `None` to drop):

```python
from mvf_sdk import run_processor, blob

def transform(payload, meta):
    return blob(bytes(255 - b for b in payload.memory))   # invert every byte

run_processor("py.invert-transformer", transform)
```

Classifiers use `run_classifier(id, fn)` returning `(label, measurement, unit, details)`.
Full example: [`modules/py-invert-transformer/`](../modules/py-invert-transformer).

## C++ — link `libmvf_sdk`

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

Full example + build: [`src/sdk/cpp/`](../src/sdk/cpp).

## .NET — reference `MachineVisionFabric.Sdk`

In-process .NET modules derive from a base class and describe their typed ports:

| Base class | Node kind |
|---|---|
| `FrameSourceModuleBase<TOptions>` | camera / stream / folder source |
| `FrameProcessorModuleBase<TOptions>` | processor / filter |
| `FrameClassifierModuleBase<TOptions>` | classifier |
| `FrameSinkModuleBase<TOptions>` | sink (dataset writer, PLC output) |
| `ProductPresenceGateModuleBase<TOptions>` | control-flow gate |

```csharp
public sealed class BrightnessGateModule : FrameProcessorModuleBase<BrightnessGateOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateProcessor<BrightnessGateOptions>(
            "mvf.example-brightness-gate", "Brightness Gate", "1.0.0", "brightness-gate",
            "Accepts a frame only when its mean byte value meets a threshold.");

    protected override IFrameProcessor CreateProcessor(BrightnessGateOptions o) => new Gate(o);
    // IFrameProcessor.EvaluateAsync returns a FrameProcessorDecision (accept/reject)
}
```

Helpers: `IntegrationModuleDescriptorBuilder`, `FrameEnvelopeFactory`, `PackagePathResolver`,
`BackgroundFrameSourceSession`. Full example:
[`modules/dotnet-brightness-gate/`](../modules/dotnet-brightness-gate).

## Boundary rule

If a module needs a vendor DLL or station-specific assumptions, it is a separate integration
module built on the SDK and loaded by the runtime — it does **not** go into `src/`. If a
feature changes graph execution semantics rather than talking to a device, it belongs in the
engine, not the SDK.
