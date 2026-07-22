# Session handoff — 2026-07-22

Paste the block below into a fresh Claude Code session in this repo to continue. Full
detail lives in `docs/roadmap.md`, `docs/data-plane-design.md`, `protocol/README.md`.

---

You are continuing **MachineVisionFabric** (`~/Projects/machine-vision-fabric`), an
open-source .NET 10 **strictly-typed graph pipeline engine**. Read `docs/roadmap.md` and
`docs/data-plane-design.md` FIRST — canonical plan + design. Also `protocol/README.md`.

**Thesis:** not a machine-vision product — a general, rule-governed engine for **easy
integration of heterogeneous nodes into typed pipelines** ("write a node once, reuse
everywhere"). Headless-first; polyglot modules (.NET in-process; Python/Node out-of-process).
**Local only, NO network** — shared-memory data plane + stdio control plane. Keep the
**data-edge vs control-edge** split sharp; do NOT drift into a generic any-payload bus or a
general Node-RED clone — the typing discipline is the edge. Don't over-polish the existing
example modules; keep momentum forward.

**Branch:** `fix/cli-default-paths-ux` (name is a misnomer; holds all recent work). Never
commit to main — branch/push, leave merge to me. Verify by building AND running, not just
compiling. Commit messages end with the `Co-Authored-By: Claude Opus 4.8` line. Keep
`docs/roadmap.md` statuses updated.

**Env quirk:** `global.json` pins SDK 10.0.300 but this machine has 10.0.101. To build/test:
temporarily set `{"sdk":{"version":"10.0.101","rollForward":"latestFeature"}}`, then restore
10.0.300 before committing.

**Layout (north-star, done):** `src/core/Mvf.Graph` (typed graph model + validation),
`src/core/Mvf.Abstractions` (contracts + interfaces), `src/engine/Mvf.Engine` (scheduler +
node runners), `src/sdk/dotnet/Mvf.Sdk`, `src/cli/Mvf.Cli`, `src/hosting/Mvf.Hosting.Worker`
(out-of-process worker host). `modules/` (Cognex camera, dark-frame-filter, black-screen-check,
dataset-writer = .NET; py-brightness-classifier = Python), `packages/cognex-dark-capture`,
`protocol/`, `tools/Mvf.SchemaExporter`. Solution `Mvf.slnx`. Module plugin namespaces stay
`MachineVisionFabric.Integrations.*` (manifest string-coupling).

**Done:** typed graph engine + `execute-graph`; runners incl. frame→control **classifier**;
legacy runner/Host/Storage removed; north-star rename; **M1 slice 1** (Python classifier runs
out-of-process over stdio JSON, plugs into `IFrameClassifier` via `Mvf.Hosting.Worker`; e2e
test green); **unified minimal `module.json`** = `{id,name,version,kind,runtime,entry}` (ports
derived from `kind`; .NET entry type auto-found). Tests 30/30.

**Next work, in order:**
1. **Lean pipeline authoring (shape approved).** Add a `PipelineExpander` that expands a lean
   pipeline.json to the current rich model BEFORE validation, so validator/executor stay
   UNCHANGED. Target shapes:
   - module node: `{ "id":"blackCheck1", "module":"mvf.black-screen-check", "config":{...} }`
   - primitive node: `{ "id":"fork1", "primitive":"fork" }` (fork/switch outputs derived from
     the edges leaving the node)
   - edge: `{ "from":"camera1.frame", "to":"fork1.frame" }` (id + kind auto)
   Derivation: read a lightweight `module.json` catalog (no DLL load) → `kind` → standard ports
   + category (source→source, processor→compute, classifier→classify, gate→control, sink→output).
   Wire the expander into CLI `validate-pipeline` + `execute-graph`; rewrite
   `packages/cognex-dark-capture/pipeline.json` lean.
2. **Auto-wire python modules.** A module catalog exposing `runtime`/`kind` so the activator,
   for a `runtime:"python"` classifier node, spawns the worker and builds a
   `WorkerFrameClassifier` → existing `FrameClassifierNodeRunner`. Add a Python classifier node
   to a demo pipeline and run it.
3. **M2 shared-memory data plane — GATED. Do NOT implement before discussing the design with
   me.** Decisions agreed in `docs/data-plane-design.md` (engine-owned variable-size arena;
   refcounts precomputed from the static graph; descriptor in slot header; engine context slot;
   engine-allocated module state; cycle-boundary snapshots + resume-after-crash). Open impl
   decisions: allocator strategy, snapshot mechanism, SDK state surface.

---
