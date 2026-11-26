# mvf-sdk (Python)

The Python SDK for authoring **MachineVisionFabric** modules.

MachineVisionFabric is an open-source, edge-first vision pipeline platform. A
module is an out-of-process node in a strict typed graph: the **control plane**
is newline-delimited JSON over stdio, and the **data plane** is a shared-memory
arena — payloads are typed and byte-based (`[descriptor | bytes]`), read and
written **in place with zero copy**. There is no base64; bytes never travel
inline. See the repository `protocol/README.md`.

## Writing a module

A classifier returns `(label, measurement, unit, details)`:

```python
from mvf_sdk import run_classifier

def classify(payload, meta):
    mean = sum(payload.memory) / len(payload.memory)
    return ("black" if mean < 10 else "ok"), mean, "mean-byte", None

run_classifier("py.brightness-classifier", classify)
```

A processor (transformer) returns a new typed payload, or `None` to drop:

```python
from mvf_sdk import run_processor, blob

def transform(payload, meta):
    return blob(bytes(255 - b for b in payload.memory))

run_processor("py.invert-transformer", transform)
```

An analyzer returns a standard multi-output result:

```python
from mvf_sdk import analysis, blob, run_analyzer

def analyze(payload, meta):
    frame = bytes(payload.memory)
    mean = sum(frame) / len(frame) if frame else 0.0
    label = "black" if mean < 10 else "ok"
    return analysis(
        frame=blob(frame),
        label=label,
        measurement=mean,
        unit="mean-byte",
        value={"mean": mean, "label": label},
    )

run_analyzer("py.brightness-analyzer", analyze)
```

Optional `on_start` (warmup / readiness), `on_checkpoint` and `on_restore`
(durable state across restarts) are supported — see the docstrings.

## Presenting frames (Display Fabric, Rust runtime)

A node whose contract declares `permissions.display.mode: managed-surface` gets shared-memory
surfaces; a display host in the user's session shows them. No UI toolkit is needed:

```python
from mvf_sdk import display

win = display.surfaces()[0]                 # empty list when the node runs headless
win.present(payload.memory, width, height, "rgb8")   # never blocks; False on standby
for event in win.poll_input():              # only what the contract grants (keyboard/mouse)
    if event.kind == "pointer-down":
        roi = (event.x, event.y)            # frame pixels
```

See `modules/py-camera-preview` and `protocol/display-surface.md`.

## Install

```bash
pip install mvf-sdk            # core, no deps
pip install "mvf-sdk[numpy]"   # + numpy for Payload.numpy() tensor views
```

## Related SDKs

The same protocol has .NET (`MachineVisionFabric.Sdk`) and C++ SDKs.

## License

Apache-2.0
