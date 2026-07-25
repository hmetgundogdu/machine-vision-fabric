# Loop — the graph's iteration authority

Status: **built** (2026-07-24), serial mode. Builds on
[value-and-select-design.md](value-and-select-design.md), which is built.

## The two planes, and where the loop lives

The engine is a pure executor: it calls nodes in topological order and repeats. No flow policy lives in it.

- **Data plane — a strict DAG.** Frames only ever flow forward (cam → … → sink); no data edge ever cycles.
  These edges are strictly typed, port to port.
- **The loop closes by id.** The pipeline's tail connects back to the loop with a plain **node-level** edge
  — `save → cycle`, by id, no port. It moves no value, so it needs no typed port: forcing it through the
  port-to-port model (inventing a `sink.done → loop.done`) was ceremony and is gone. The loop edge is
  recognised by its target being a loop; the validator checks only that both nodes exist.

```
   loop  ◀────────── save → cycle ──┐   (node-level edge, by id — closes the loop)
    │                               │
    ▼ (owns iteration + pause)      │
   cam ──frame──▶ … ──frame──▶ save ┘   (data — strict DAG, typed, always forward)
```

There is **no setup region**. Every node runs every cycle. The loop edge is declarative — it draws the
close; the loop's *logic* is its `mode`, so a loop with no edge at all still works.

## What the loop owns

The `loop` node is the graph's **iteration authority**. Two responsibilities, nothing else:

1. **Termination policy** (`mode`):
   - `until-exhausted` — run until the source stops producing (a folder played once). Default.
   - `forever` — never stop on exhaustion; a finite source is **rewound** by the loop and replayed. A live
     camera never exhausts, so `forever` just runs. This is what a source-level `loop:true`/replay flag
     used to do — moved to the one place that owns iteration, so there is no second "loop" knob.
   - `count` — stop after `count` cycles.
2. **Running state** (pause). Whole-graph, on the executor's `RunControl` (held on `LiveValueRegistry`).
   Space toggles it in the TUI; the executor idles while paused — process alive, workers warm, resume
   continues where it left off. **Pause is not cancel:** nothing in flight is torn down. Cancellation (a
   cross-language cooperative token) is a separate, heavier mechanism on the module-lifecycle L-track.

The loop's `done` input is optional and multi-edge: a bare loop (no `done` wired) still declares the run
repeats and carries pause; several sinks can close into one loop.

## How `forever` rewinds

On source exhaustion, if `mode` is `forever` and the pass actually produced frames, the executor rewinds
every **rewindable** source (`IRewindableSource`) and continues — the empty exhaustion cycle is not counted.
`FrameSourceNodeRunner` rewinds by re-opening its session's stream from the start. A source that cannot
rewind is left exhausted, so `forever` degrades to `until-exhausted` for it rather than spinning. A pass
that produced nothing is never rewound (an empty folder cannot loop forever making no progress).

## Live bindings from the CLI

A running pipeline's declared values are editable live, from the CLI — no hot graph-swap, just the values.
Two kinds, both type/schema-checked and persisted to the binding store:

- **`value` nodes** — the node re-reads the registry each cycle; an edit lands on the **next cycle**, no
  restart. (Built earlier.)
- **Module config** — a node declares live-editable fields in a `bindings` map (each with a `type`, an
  optional `schema`, and a persistence `binding` key). A module reads its config only at activation, so an
  edit **re-activates** the node with the new value: the executor watches the registry and, at the next
  cycle boundary (a quiesced point), merges the value into config and re-opens the node. The node restarts
  (a source rewinds to its first frame) — the accepted cost of not touching the module contract. The
  pre-pass overlays any stored edit before the run, so yesterday's change comes back today.

```json
{ "id": "cam", "module": "mvf.folder-source",
  "config":   { "frameIntervalMs": 300 },
  "bindings": { "frameIntervalMs": { "type": "int", "binding": "cam.interval",
                                     "schema": { "type": "integer", "minimum": 0, "maximum": 5000 } } } }
```

Reuses everything the `value` path already had — `LiveValueRegistry`, the dashboard editor, the binding
store, `ControlValueType` + `JsonSchemaCheck`. The only new engine behaviour is re-activation on change.
Code: `ModuleBinding`/`ModuleBindings` (Mvf.Engine.Values), `PipelineNodeDefinition.Bindings`, the
expander/validator/pre-pass/activator, and `PipelineGraphExecutor` (the reactivation queue + drain).

## Where the code is

| piece | file |
|---|---|
| mode + config | `LoopMode` (Mvf.Graph.Values), `LoopPrimitiveConfig` (Mvf.Engine.Values) |
| ports | none — the loop is portless |
| loop edge (by id) | `PipelineExpander` (`save → cycle`, kind `loop`, no ports); `PipelineDefinitionValidator.ValidateEdge` skips port/type checks for it |
| validator | `PipelineDefinitionValidator.ValidateLoopNode` — mode + count |
| iteration | `PipelineGraphExecutor` — `forever` rewind, `count`, pause gate |
| rewind | `IRewindableSource` / `FrameSourceNodeRunner.RewindAsync` |
| CLI | `PipelineDashboard` — space toggles, header shows PAUSED |
| demo | [`packages/loop-demo`](../packages/loop-demo/pipeline.json) — `forever`, `save.done → cycle.done`, press space |

Pipelined mode **rejects** a `loop` up front (pause is serial-only for now), like `value`/`select`.

## Still open (deferred, not blocking)

- **Per-frame live module config** — the re-activation model restarts the node on every edit (chosen for
  zero contract change). A smooth, no-restart path would pass modules a live-config handle they read each
  frame — a broader contract change, a later option if the restart proves too coarse.
- **Module bindings from env / interactive resolve** — today they resolve from the store only; the value
  path also does env + prompt, which could be unified.
- **Cancellation tokens** — the cooperative, cross-language stop. Separate gate on the L-track.
- **Per-loop pause / multiple loops** — whole-graph is enough while graphs carry one loop.
- **The forward `tick` (loop → head)** — only the `done` back-edge is wired today; a full drawn cycle
  (loop also ticking the head) would need feedback-value persistence and is not needed yet.
