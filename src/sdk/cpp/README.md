# MVF C++ SDK

The C++ SDK for authoring **MachineVisionFabric** modules. It mirrors the
reference Python SDK (`src/sdk/python/mvf_sdk`) and speaks the exact same wire
protocol, so a C++ module is interchangeable with a Python or .NET one.

- **Control plane**: newline-delimited JSON over the module's stdio (`protocol/README.md`).
- **Data plane**: a shared-memory arena addressed by `MVF_ARENA_PATH`; payloads are
  typed and byte-based (`[192-byte descriptor | bytes]`), read and written **in place,
  zero copy**. No base64; bytes never travel inline.

A module is a small executable that links this SDK. The engine launches it as an
out-of-process node — **no engine changes are required** to add a C++ module.

## Author API

```cpp
#include "mvf/sdk.hpp"
using namespace mvf;

int main() {
    // Processor: frame in -> new frame out (return std::nullopt to drop).
    return run_processor("cpp.invert-transformer",
        [](const Payload& in, const json& meta) -> std::optional<Output> {
            std::string out(in.size, '\0');
            for (size_t i = 0; i < in.size; ++i)
                out[i] = static_cast<char>(255 - in.data[i]);
            return blob(std::move(out));                    // or tensor(bytes, elem, shape)
        });
}
```

Classifiers use `run_classifier(id, fn)` where `fn` returns a `Classification`
(`label` + optional `measurement` / `unit` / `details`). Optional lifecycle hooks —
`on_start` (warmup / readiness), `on_checkpoint`, `on_restore` — are passed via
`ModuleHooks`.

## Build

```bash
cmake -S src/sdk/cpp -B build/cpp -DCMAKE_BUILD_TYPE=Release
cmake --build build/cpp
```

Outputs the shared library:

| Platform | Artifact |
|---|---|
| Linux | `libmvf_sdk.so` |
| macOS | `libmvf_sdk.dylib` |
| Windows | `mvf_sdk.dll` (+ import lib `mvf_sdk.lib`) |

plus the `mvf_invert_transformer` example. The only third-party dependency is the
single-header [`nlohmann/json`](https://github.com/nlohmann/json), fetched at
configure time (or pass `-DMVF_SDK_SYSTEM_JSON=ON` to use an installed copy).

## Requirements

- CMake ≥ 3.16, a C++17 compiler.
- Little-endian host (x86-64 / arm64), matching the on-wire descriptor layout.

## License

Apache-2.0
