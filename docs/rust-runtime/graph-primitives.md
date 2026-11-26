# Graph primitives, the loop, values and live operation (Rust runtime, #25)

The Rust runtime implements the .NET graph primitives with the same semantics. The .NET design docs
remain the reference: [value-and-select-design.md](../value-and-select-design.md) and
[loop-and-running-state-design.md](../loop-and-running-state-design.md). This page covers the pipeline
syntax, how the primitives are typed, and the places where Rust deliberately differs.

## Syntax

```yaml
spec:
  nodes:
    - { id: cycle, primitive: loop, config: { mode: forever } }      # until-exhausted | forever | count
    - id: thr
      primitive: value
      config: { type: int, binding: brightness.threshold, literal: 40,
                schema: { type: integer, minimum: 0, maximum: 255 } }
    - id: cam
      builtin: folder-source
      config: { sourceFolder: frames, frameIntervalMs: 250 }
      bindings:                                                       # live-editable module config
        frameIntervalMs: { type: int, binding: cam.interval, schema: { type: integer, minimum: 0 } }
    - { id: fork,  primitive: fork }
    - { id: route, primitive: switch }
    - { id: when,  primitive: if }
    - { id: pick,  primitive: select, config: { mode: one, by: serial, binding: camera.serial } }
  edges:
    data:    [ { from: cam.frame, to: fork.frame }, { from: fork.toRoute, to: route.frame } ]
    control: [ { from: gate.ok, to: when.productPresent } ]
    loop:    [ { from: save, to: cycle } ]                            # node ids; moves no value
```

## Typing

Each primitive gets a synthesized Universal Node Contract (`mvf.primitive.<kind>`). After that, the
same edge rules apply as for every other node.

| primitive | inputs | outputs |
|---|---|---|
| `fork` | `frame: frame` | every port its outgoing data edges name (`frame`) |
| `if` | `frame: frame`, `productPresent: control signal` | `acceptedFrame: frame` |
| `switch` | `frame: frame`, `class: classification` | every port its outgoing edges name; `default` is the fallback |
| `value` | — | `value`: `value` port for a scalar, `json` port for `json` or `shape: list` |
| `select` | `items: json` (list), `criterion` (optional, the element type) | `selected`: element in `mode: one`, list in `mode: many` |
| `loop` | — | — (portless; owned by the executor, never executed) |

Between two primitives, the graph also checks the refined value type: element type plus shape,
spelled `value:int` or `list:json` (the .NET `control/value:<t>` and `control/list:<t>`). A
mismatch is reported as `graph.edge.value-type-mismatch`. Schemas are enforced with the .NET
subset: `type`, `enum`, `const`, `properties`, `required`, `additionalProperties`, `items`,
`min/maxItems`, numeric bounds, `min/maxLength` and `pattern`. Unknown keywords are ignored. For a
list, the schema applies to each element, and errors carry a path such as
`$[1]: missing required property 'protocol'`.

## Semantics

- **Absence.** A node whose required input is absent, or whose connected control input is absent or
  false, is skipped. Its outputs stay absent for that cycle. This gives the .NET `NoOutput`
  behaviour for a closed `if`, an untaken `switch` branch and a `select` with no match.
- **Loop.** Every node runs every cycle; there is no setup region.
  - `forever`: when a source runs dry, every rewindable source is rewound and the cycle runs again
    from frame 1. The empty pass is not counted. A pass that produced nothing is never rewound.
  - `count`: the run ends after N cycles with `EndReason::LoopCompleted`. Like an exhausted source,
    this clears the checkpoint.
  - A source that cannot rewind leaves `forever` behaving like `until-exhausted`.
- **Pause.** Pause is the loop's running state and applies to the whole graph. While paused, no
  node runs, no cycle is counted, the process stays alive and workers stay warm. Pause is not
  cancel. A graph without a `loop` refuses pause.
- **Values.** Resolution order is `literal` → binding store (`.mvf/bindings.json`) →
  `MVF_BINDING_<KEY>` → `default`. An environment answer is never cached in the store. A `select`
  without a wired criterion or `where` must resolve its binding before cycle 1.
- **On-demand.** A node with `lifecycle.activation: on-demand` starts the first time it runs.
  Restored checkpoint state is handed to it at that moment.

## Live operation (cycle boundaries)

| operation | API (`ControlHandle`) | update-plan step | effect |
|---|---|---|---|
| set a value | `set_value(node, v)` | `{ node, atCycle, action: set, value }` | `value` and `select` only. Type- and schema-checked. The next cycle sees it, with no restart. Persisted to the node's binding. Refused for a `select` whose criterion comes from an edge. |
| edit a module binding | `configure(node, field, v)` | `{ node, atCycle, action: configure, field, value }` | Checked against `bindings`. Applied live when the node can take it: workers that advertise `configure` get the new config in the same process. Otherwise the node is reopened with the new config (a source restarts at frame 1). Persisted. |
| pause / resume | `pause(for_ms)`, `resume()` | `{ node: <loop>, atCycle, action: pause, forMs }`, `action: resume` | Whole-graph pause, optionally with automatic resume. |

Every operation is recorded in `RunReport.live`; the `loop` counters are in `RunReport.iteration`.

## Differences from .NET (deliberate)

| .NET | Rust | why |
|---|---|---|
| `value`/`select` signals travel on **control** edges | they travel on **data** edges (`value`/`json` ports) | In the Rust contract, `signal` is the only control type, and control means "does this node run". Values are data, so the data/control split stays sharp. |
| `switch.class` is a control classification | `switch.class` is a data `classification` | Same reason. The routing decision itself (which output is present) is the control flow. |
| An unresolved binding prompts an operator (TUI/terminal) | The run fails before cycle 1, naming the binding, the store path and `MVF_BINDING_<KEY>` | The Rust host is unattended. There is no TUI in scope. |
| On-demand activation failure: warn, then skip the node forever | handled by the node's `onError` (default `fail`; `skip` reproduces .NET) | Nothing fails silently unless configured. |
| An edited module binding always reopens the node, except for `ILiveConfigurable` nodes | A worker that advertises `configure` takes it live; any other node is reopened | Same model; the Rust worker protocol already carries `configure`. |
| Pipelined mode rejects `loop`, `value`, `select` and on-demand nodes | see [stage 13](stages/13.md) | |
| Node ids are `camelCase` | Node ids are lower-case (`save-black`); port names may be camelCase | The Rust contract id rule. |

## Evidence

- `crates/mvf-engine/tests/primitives.rs`: 38 tests. Names follow the .NET primitive, loop,
  value/select, pre-pass, module-binding and on-demand tests.
- `crates/mvf-graph/tests/primitives.rs`: 8 tests covering typing and every validation code.
- `crates/mvf-worker/tests/recovery.rs`: live `configure` in the same Python process, and a reopen
  when the worker refuses it.
- [`rust/scripts/parity-demos.sh`](../../rust/scripts/parity-demos.sh): runs `packages/{inspection,loop,value}-demo`
  on the .NET engine and their twins `rust/examples/{inspection,loop,value}-demo` on the Rust
  runtime, then compares every dataset file by name and SHA-256 (see [stage 12](stages/12.md)).
