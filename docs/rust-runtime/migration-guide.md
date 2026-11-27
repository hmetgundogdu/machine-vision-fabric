# Migrating a legacy pipeline to `mvf`

`mvf` is the MVF runtime. The legacy .NET runtime (engine, CLI, in-process SDK and modules,
egress) is archived (#34):

- **Code:** branch `archive/dotnet-legacy`, tag `dotnet-legacy-final`.
- **Last release that ships it:** v1.1.0, which has the legacy CLI as `mvf-cli-<os>` and the Rust
  runtime as `mvf-rs-v1.1.0-<os>.zip`. From v2.0.0 the binary is called `mvf`.

Nothing below changes a legacy package. The importer writes `pipeline.yaml` next to
`pipeline.json`, so the legacy CLI from v1.1.0 can still run the same package for comparison or
rollback.

## 1. Install

Download `mvf-<version>-<os>.zip` from the release and unpack it. It holds:

- `mvf`;
- `mvf-remote` (TLS relay, package signing);
- `mvf-display-host`.

No .NET, Docker or CLR is needed to run them. Python modules need Python 3 and the MVF Python SDK
(`pip install mvf-sdk`), as they did under .NET.

## 2. Convert

```text
mvf import packages/my-line                 # writes packages/my-line/pipeline.yaml, then validates it
mvf import packages/my-line --modules ../modules --force
```

What the importer does, and refuses:

| Legacy | Rust | |
|---|---|---|
| `"id": "saveOk"` | `id: save-ok` | Ids must be lowercase contract ids; edges follow, and every rename is printed |
| `"module": "mvf.folder-source"` | `builtin: folder-source` + `cameraId: sim-cam-1` | The id the .NET simulator stamps, so dataset metadata stays the same |
| `"module": "mvf.dataset-writer"` | `builtin: dataset-writer` | |
| `"module": "py.…"` (or any module with a `node.yaml`) | `module: py.…` | Resolved in the module roots, run as a worker, as under .NET |
| `"module": "mvf.realworld-cognex-camera"`, `mvf.synthetic-camera`, other .NET in-process integrations | reported **MISSING**, exit 1 | Under Rust, .NET code runs only as a worker. Port it to the `Mvf.Worker` SDK with a `node.yaml` (step 3) |
| `"primitive": "fork" / "switch" / "select" / "value" / "loop" / "if"` | the same primitive | Configs carried over as they are |
| `"bindings"` | `bindings` | Same shape and same binding keys, so stored values still apply |
| `{ "from": "a.p", "to": "b.q" }` | `edges.data` or `edges.control` | By the ports' channel in the contracts |
| `{ "from": "save", "to": "cycle" }` | `edges.loop` | A port-less edge must close into a `loop`; anything else is refused |
| (implicit .NET failure behaviour) | `run.onError: { sources: fail, nodes: skip }` | A failing source ends the run and a failing mid-graph node is skipped, as in .NET. The Rust default would fail on any node |
| `version`, `displayName` | header comment | Not part of the Rust schema |
| any other field | **refused**, exit 2 | Nothing is dropped silently |

## 3. Modules the importer reports as MISSING

- **Python modules** need a `node.yaml` contract in a module root. Every module under `modules/py-*`
  has one. A module of your own that only has `module.json` needs one written: see
  [node-contract.md](node-contract.md) and the `py-*` examples.
- **.NET in-process integrations** (for example a vendor camera) cannot be loaded into the Rust
  process. Wrap the existing C# logic in a worker with `src/sdk/dotnet/Mvf.Worker` (examples in
  `src/sdk/dotnet/examples`), publish it, and give it a `node.yaml`. The business logic stays
  C#; only the host changes.
- **Vendor hardware** that no worker exists for yet is a blocker, not something to work around.
  Keep that line on the legacy runtime until the worker is done.

## 4. Settings and bindings

- **Bindings and values.** The .NET CLI asks for a missing binding. `mvf` never prompts: it
  reads `<package>/.mvf/bindings.json` or `MVF_BINDING_<KEY>`, and otherwise names the missing key
  before the first cycle. Example: `MVF_BINDING_CAMERA_SERIAL=DEF456`.
- **Pacing** (`frameIntervalMs`) and relative paths (`sourceFolder`, `outputRoot`) mean the same
  as before, because `pipeline.yaml` sits in the package directory.

## 5. Run

| .NET CLI | mvf |
|---|---|
| `execute-graph --package P --max-cycles N` | `mvf run P/pipeline.yaml --max-cycles N` |
| `--integrations-root modules` | `--modules modules` (default: the nearest `modules/` above the package) |
| `validate-pipeline --path P/pipeline.json` | `mvf validate P/pipeline.yaml` |
| live TUI dashboard | `mvf run … --tui`, or `mvf run … --manage` and `mvf manage tui` ([TUI guide](tui.md)) |
| `modules`, `packages` | `mvf host` (builtins and backends) |
| stage-parallel run | `mvf run … --mode pipelined` |

## 6. Validate before switching over

Run both runtimes on the same input and compare what they write. For the demo packages this is
automated. The script checks out the archived engine (tag `dotnet-legacy-final`) into
`.legacy-dotnet/`, builds it, and converts each legacy package with the importer:

```text
RUST_SOURCE=import rust/scripts/parity-demos.sh
RUST_SOURCE=import RUST_MODE=pipelined rust/scripts/parity-demos.sh
```

It compares every dataset file by SHA-256, plus per-frame metadata.

For a real line, do the same with its recorded frames:

1. Point a folder source at them.
2. Run the legacy CLI (`mvf-cli-<os> execute-graph`, from v1.1.0) and `mvf run`.
3. Compare the two `out/` directories.

Then run `mvf` in shadow, beside the production runtime with outputs to a separate folder, for a
soak period before switching.

## 7. Rollback

- The legacy package is untouched. Run it with the v1.1.0 legacy CLI (`mvf-cli-<os>`), or build
  it from the archive: `git worktree add ../mvf-legacy dotnet-legacy-final`.
- Delete `pipeline.yaml`, or keep it: the legacy CLI only reads `pipeline.json`.
- Binding values in `.mvf/bindings.json` are shared, so a value edited under one runtime is seen
  by the other.

## Known differences

- **The TUI is a management client** ([tui.md](tui.md)). It is the legacy dashboard's picture,
  but it never shows config values, and worker CPU/memory is not in it yet. `mvf` never
  prompts: missing bindings come from the store or the environment.
- **Live edits** come from the TUI's editable fields, an update plan (`--updates`), or
  `mvf manage command`.
- **Realtime egress is not ported.** `--egress tcp|ws|udp` on the .NET CLI (live state and frame
  data to external viewers) has no Rust equivalent. The management feed and previews cover
  observation, not frame-data publishing. A line that depends on egress stays on the legacy
  runtime for that part.
- **Not available yet:** single-host OCI + GPU (#26) and the Windows OCI path (#27) are BLOCKED
  on hardware. Native and worker nodes run on Windows, Linux and macOS.
- The full feature-by-feature comparison is in [parity-matrix.md](parity-matrix.md).
