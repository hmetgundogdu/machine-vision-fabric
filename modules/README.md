# Modules

Worker modules the `mvf` runtime finds by contract id. Each directory holds a `node.yaml`
contract, and `mvf run` searches the nearest `modules/` above a pipeline, or `--modules DIR`.

| Module | Kind |
|---|---|
| `py-invert-transformer` | Python transformer |
| `py-brightness-classifier`, `py-brightness-analyzer`, `py-frame-counter`, `py-warmup-classifier` | Python analyzers and classifiers |
| `py-camera-preview` | Python display node (Display Fabric, #30) |
| `py-ftp-source` | Python source over the network (Network Fabric, #23) |
| `py-model-inspector` | Python model and storage example (Storage Fabric, #31) |
| `py-bench-touch`, `py-bench-numpy` | benchmark workloads |

- **Python** modules use the SDK in `src/sdk/python`.
- **C# nodes** are worker executables built with `src/sdk/dotnet/Mvf.Worker` (examples in
  `src/sdk/dotnet/examples`).
- **C++ nodes** use `src/sdk/cpp`.

The legacy .NET in-process integrations (Cognex camera, dark-frame filter, black-screen check,
folder source, dataset writer, synthetic camera) are archived with the legacy runtime: branch
`archive/dotnet-legacy`, tag `dotnet-legacy-final` (#34). The folder source and dataset writer are
`mvf` builtins. The others need a `Mvf.Worker` port to run under `mvf`.
