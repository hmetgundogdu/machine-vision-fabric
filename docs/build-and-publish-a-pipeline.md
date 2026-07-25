# Build and publish a pipeline

End-to-end journey for an outside developer: author a node with the SDK, wire it into a
typed pipeline, run it on the CLI, and publish a self-contained deployment. Everything here
is **hardware-free** — the demo modules use simulator sources.

This guide stitches together two existing references:

- [`sdk-quickstart.md`](sdk-quickstart.md) — writing a module (the deep dive).
- [`cli-guide.md`](cli-guide.md) — driving the CLI and the live dashboard (the deep dive).

> In the examples a downloaded release binary is just `mvf`. From a source checkout, replace
> `mvf` with `dotnet run --project src/cli/Mvf.Cli --`.

---

## The mental model

A **pipeline** is a strict, typed DAG of **nodes** connected by **edges**:

```text
NODE(typed output) -> NODE(typed input) -> NODE(typed output)
```

- **Nodes** are either engine **primitives** (`loop`, `fork`, `switch`, `value`, `select` —
  owned by the runtime) or **integration modules** you author with the SDK (camera, filter,
  classifier, sink).
- **Edges** are typed and split into **data** (frame/tensor transfer) and **control** (a PLC
  presence decision, a branch selection). See
  [`pipeline-graph-foundation.md`](pipeline-graph-foundation.md).

A shippable **package** is a folder: a `pipeline.json` plus any `assets/` (frames, models,
scripts, configs).

---

## 1 · Author a node with the SDK

Install the SDK for your language, then implement one typed work node. All three SDKs speak the
same protocol, so a module is interchangeable across languages.

- **.NET** — NuGet `MachineVisionFabric.Sdk`. Derive from one of the base classes in
  `src/sdk/dotnet/Mvf.Sdk/`:
  - `FrameSourceModuleBase<TOptions>` — camera / stream / folder source
  - `FrameProcessorModuleBase<TOptions>` — transform / filter
  - `FrameClassifierModuleBase<TOptions>` — classification → control signal
  - `FrameSinkModuleBase<TOptions>` — dataset writer / output
  - `ProductPresenceGateModuleBase<TOptions>` — control-flow gate
- **Python** — `pip install mvf-sdk`, then `run_processor(...)` / `run_classifier(...)`.
- **C++** — link `libmvf_sdk` (`src/sdk/cpp/include/mvf/sdk.hpp`).

Nodes stay **fully typed**: typed options, typed capability kind, typed input/output ports. The
SDK is for **work nodes** only — flow-control primitives live in the engine, not the SDK.

## 2 · Declare it in `module.json`

Every module ships a `module.json` next to its entry point so the CLI can discover it:

```json
{
  "id": "mvf.folder-source",
  "name": "Folder Sequence Source",
  "version": "1.0.0",
  "kind": "source",
  "runtime": "dotnet",
  "entry": "MachineVisionFabric.Integrations.FolderSource.dll"
}
```

The `id` is what a pipeline node references. Confirm discovery:

```bash
mvf modules            # lists every module.json under the integrations root
```

## 3 · Author `pipeline.json`

Create a package folder and describe the typed graph. Schema lives in
`src/core/Mvf.Graph/Pipelines/Pipeline*.cs`:

```json
{
  "name": "my-pipeline",
  "version": "1.0.0",
  "nodes": [
    { "id": "cam",  "module": "mvf.folder-source", "displayName": "Folder Source",
      "config": { "sourceFolder": "assets/frames", "frameIntervalMs": 300 } },
    { "id": "save", "module": "mvf.dataset-writer", "displayName": "Save",
      "config": { "outputRoot": "out", "sessionPrefix": "demo" } }
  ],
  "edges": [
    { "from": "cam.frame", "to": "save.frame" }
  ]
}
```

- `nodes[].module` references a `module.json` `id`; `nodes[].primitive` selects an engine
  primitive instead.
- `edges[]` use `from`/`to` = `node.port`. Add `"kind": "control"` for control flow (data is
  the default). A tail edge with no port (`"from": "save", "to": "cycle"`) closes a `loop`.
- `bindings` on a node expose live-tunable fields the dashboard can edit mid-run.

See `packages/loop-demo/pipeline.json` and `packages/value-demo/pipeline.json` for worked
examples.

## 4 · Add assets

Drop simulator frames, models, scripts, or configs the pipeline needs into the package:

```text
my-pipeline/
├── pipeline.json
└── assets/
    └── frames/            # simulator source data
```

## 5 · Validate

Type-check the graph (typed ports, data/control edges, DAG) without running it:

```bash
mvf validate-pipeline --path my-pipeline/pipeline.json
```

## 6 · Run

```bash
mvf execute-graph --package my-pipeline                 # live TUI dashboard
mvf execute-graph --package my-pipeline --no-tui --max-cycles 3   # headless
```

In the dashboard: `←/→` move between nodes, `Enter` opens node detail, `Space` pauses/resumes,
and typing edits a `value` node or a module binding live. See [`cli-guide.md`](cli-guide.md).

## 7 · Read back the run

```bash
mvf sessions                              # finished runs + output metadata
mvf inspect-session --path <session-id>   # per-sink frame counts, manifest
```

---

## 8 · Publish a deployment

`publish.ps1` assembles a self-contained folder that runs on an edge device with **no .NET
installed**:

```powershell
./publish.ps1 -Output dist/mvf -Runtime win-x64 -SingleFile $true
```

Output layout:

```text
dist/mvf/
├── Mvf.Cli(.exe)          # self-contained single-file CLI (carries the .NET runtime)
├── integrations/          # each module framework-dependent under integrations/<id>/
│   ├── mvf.folder-source/
│   ├── mvf.dataset-writer/
│   └── ...
└── packages/              # pipeline packages (JSON + folder)
```

No config file ships — the CLI is fully self-contained (see Deploy notes).

Only the CLI carries the runtime; modules are framework-dependent DLLs because the CLI host
loads them into its own runtime. Run it on the target with:

```bash
cd dist/mvf
./Mvf.Cli execute-graph --package packages/my-pipeline
```

`.github/workflows/release.yml` builds the same single-file CLI per platform
(`linux-x64`, `osx-arm64`, `win-x64`) on tag push.

---

## Deploy notes

- **No config file.** The CLI ships no `appsettings.json`: it carries code defaults for every
  setting (`MachineVisionFabricRuntimeOptions`, `DatasetCaptureOptions`) and auto-detects the
  `integrations/` folder next to the executable, so a deployment is fully self-contained.
  Overrides go through environment variables (e.g. `MachineVisionFabric__IntegrationsRoot=...`)
  or CLI flags; an explicit `--integrations-root` always wins.
- **The CLI is compressed and small.** Publish uses `EnableCompressionInSingleFile` +
  `InvariantGlobalization` + runtime feature switches (no trimming/AOT, so reflection-based
  config binding and dynamic module loading keep working). On `osx-arm64` this took the
  single-file CLI from **~79 MB to ~36 MB** (~54% smaller). The size is dominated by the
  bundled .NET runtime — OpenCV/ONNX are **not** in the CLI (they live in individual modules).
