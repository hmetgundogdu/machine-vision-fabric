# `value` and `select` — typed values the graph cannot compute

Status: **built** (2026-07-23) — primitives, types, validator rules, binding store, resolver seam, the CLI
pre-pass, and live tuning from the TUI. Serial mode only; the discovery module that fills a `select` with
real candidates is the next slice. Everything below describes what the code does.

## The problem

Two things a pipeline needs that a dataflow graph has no way to express today:

1. **A value the graph cannot compute.** A brightness threshold, an output folder, a line number, which
   camera to open. It comes from outside the dataflow — a literal, a machine-local binding, an environment
   variable, or an operator.
2. **Narrowing a collection.** A discovery node finds six cameras; the pipeline needs one. A module returns
   a list of regions; the pipeline wants the ones above a size.

These are separate concerns and get separate primitives. Producing a value has nothing to do with
collections; narrowing a collection has nothing to do with where a value came from. Merging them would
give one node with two unrelated jobs and a config where half the fields are always ignored.

The interactive camera picker that motivated this is then not a feature — it is `discover → select` with
an unresolved criterion, which is one composition among many.

## Why primitives, not modules

The engine would need generic value/selection support in the core either way — a binding store,
resolution before the cycle loop, validator typing rules. A module would add indirection on top of that
cost, and worse, would make the *graph language* depend on a plugin being installed. `if` does not need a
plugin; "this value comes from outside" should not either.

The split that keeps the core minimal:

| concern | where it lives |
|---|---|
| value/selection **semantics** | primitive (`value`, `select`) — pure graph behaviour |
| **how** a value is obtained | `IValueResolver` seam — terminal, config, env, later the studio |
| **discovering** candidates | module — it talks to hardware and protocols, and there will be many |

Same shape as `IDataPlane` and `IOutOfProcessModuleHost`: minimal core, implementations at the edges.

## `value`

Produces exactly one typed value.

```
value
  out: value : control/value:<type>    when shape = one   (default)
       value : control/list:<type>     when shape = list
  config: { type, shape?, schema?, literal?, binding?, prompt?, default? }
```

A `list`-shaped value is still **one** value the graph cannot compute — a set of candidates, a set of
allowed labels. `select` already emits a collection in `mode: many`, so there was no principled reason for
`value` not to produce one, and it is what lets a `value` feed a `select`'s items port. Until the discovery
module lands, that is also the honest stand-in for discovery: a literal list of candidates, which is
exactly the "simulator first" the project asks for everywhere else. For a list, the declared `schema`
describes an **element** — the same schema then reads the same whether a record arrives alone or in bulk.

Resolution order, first hit wins:

1. `literal` in config — a hand-authored constant, no interaction, fully portable
2. the binding store, if `binding` is set and a value was stored earlier
3. `IValueResolver` — prompt, environment variable, whatever the host provides
4. `default`

If none resolve, the run fails before cycle 0 with the binding name and how to set it. It never blocks
waiting for a human that is not there.

## `select`

Narrows a collection using a criterion.

```
select
  in : items     : control/list:<type>
  in : criterion : control/value:<type>   (optional — may instead come from config or the resolver)
  out: selected  : control/value:<type>   when mode = one
       selected  : control/list:<type>    when mode = many
  config: { mode: "one" | "many", type?, where?, by?, binding?, prompt? }
```

The criterion has the same three sources as a value: config (`where`), an edge (from a `value` node), or
the resolver. When it comes from the resolver **and** `items` is a collection, that is the interactive
picker — the resolver renders the list and stores the answer under `binding`. Every later run reads the
binding and never asks.

That is not a special case in the code. The pre-pass walks the edge into `items`; if the collection is
already knowable it passes the elements as `ValueRequest.Choices`, and a resolver that is given choices
renders a list instead of a text entry. With a `by`, what gets stored is that **property** of the chosen
element, not the whole record — which is what lets the binding survive a later discovery run returning the
same camera with different incidental fields.

Which criterion source to use is a real authoring choice, and the two read differently:

| criterion from | first run | later runs | mid-run |
|---|---|---|---|
| the `select`'s own `binding` | picker | silent | **picker again**, from the live candidates |
| an edge from a `value` node | that value's own resolution | same | tune the `value`, not the `select` |

A camera wants the first: chosen once per machine, then never asked again. A threshold-like criterion
wants the second, because turning it while frames flow is the point.

A criterion arriving on the edge wins over the static one, so a criterion that is genuinely per-frame
stays per-frame. `by` names the property to compare when the elements are objects (`item[by] == criterion`);
with no `by` an object criterion matches field by field, and a scalar one compares the element itself.

`type` is the **element** type of the collection, and is declared on the node rather than inferred from
the incoming edge — a node's ports have to be known before the graph is wired. A disagreement with the
producer is then an ordinary `pipeline.edge.data-type-mismatch`, which is the same rule every other edge
already obeys rather than a special case for this primitive.

`select` is not `switch`. `switch` routes one frame to one of several output ports by a control signal;
`select` narrows a collection to a smaller collection or a single element. Different inputs, different
outputs, no overlap.

## Types

Ordinary types, not a device-specific one. Port data types gain two families:

- `control/value:<t>` where `<t>` is `string | int | number | bool | json`
- `control/list:<t>` — a homogeneous collection of the above

A `json` value may declare a **JSON Schema** in `schema`. The schema is enforced wherever the value
enters the graph: a literal in config, an operator's input, a stored binding, or items arriving on an
edge. This is what keeps "the camera record" typed without the core knowing what a camera is — discovery
publishes `control/list:json` with a schema, and `select` narrows it.

For a `list`-shaped value the schema describes an **element** and is applied to each in turn, so an error
names the offending index (`$[1]: missing required property 'protocol'`). Every entry point — config
literal, operator's answer, stored binding, live tuning edit — goes through the one function that knows
this (`JsonSchemaCheck.TryValidateShaped`). It is one function on purpose: the list-versus-element
distinction was got wrong independently in three of those four places while this was being built.

The enforced subset is deliberately small — `type`, `enum`, `const`, `properties`, `required`,
`additionalProperties`, `items`, `minItems`/`maxItems`, the numeric bounds, `minLength`/`maxLength`,
`pattern` — and unknown keywords are ignored, as the specification requires of a validator. A schema
using `$ref` or `allOf` is therefore **accepted and under-enforced, never rejected**. That is a
deliberate trade: no schema engine in the core, and one seam (`JsonSchemaCheck`) to swap if enforcement
ever has to get stricter.

The expander computes each node's port types from its config, so the existing validator can do its job:

- a `select` whose `items` is `control/list:json` produces `control/value:json` in `mode: one`
- its `criterion` port must be `control/value:<t>` with the same `<t>` as the items
- the downstream input port must accept what `select` emits

No loose object passing: the type is declared, propagated and checked, exactly as for frames.

## Where resolution happens

Not inside the cycle loop. The graph is a per-frame loop; asking an operator once per frame is absurd and
blocking the loop on a human breaks unattended operation.

The CLI runs a **binding pre-pass** before execution starts: walk the definition, find `value` and
`select` nodes that are still unresolved, resolve them (from the binding store, or interactively), and
store the results. Execution then sees constants.

The edges are still real — they carry the declared types, the validator checks them, and the studio can
draw `discover → select → camera`. They just are not what moves the value at runtime.

This also avoids fighting the TUI: the dashboard repaints the whole screen every 120 ms, so a prompt
cannot share it. The pre-pass finishes before the dashboard starts.

The pre-pass is **idempotent** — a resolved node is left alone on a second pass — and it writes bindings
**once, at the end, and only when the whole pass succeeded**. A run that fails half way through must not
leave a partly bound machine behind.

Two things the resolver seam turned out to need, both learned by building it:

- **Not every answer belongs in the store.** An answer carries a `Persist` flag. A prompt sets it: the
  point of asking a person is never to ask them again. An environment variable does not — the variable is
  already durable, and since the store is read *before* any resolver, caching it would freeze the first
  run's configuration into the machine and silently ignore every later change to the variable.
- **Unattended is the default posture.** `--no-prompt` disables the terminal resolver, and it disables
  itself anyway when stdin is redirected or the terminal is not interactive. An unresolved value then
  fails the run before cycle 0 with the binding name and the file to set it in, which is the whole point:
  never block on a human who is not there.

Resolution order in the CLI is environment variable, then terminal prompt — a machine-supplied value
outranks asking a person, so a deployed panel PC never depends on someone being at the keyboard. The
variable for binding `camera.address` is `MVF_BINDING_CAMERA_ADDRESS`.

## Live tuning

A resolved value is a constant, but not a frozen one. Each `value` node registers a `LiveValue` in a
`LiveValueRegistry`, and the TUI can change it while the run is in flight — tab to it, enter, type a new
setting. The node reads the registry once per cycle and picks the change up on its **next** pass.

This does not undo the reason prompting was moved out of the loop. That reason was never "values must not
change"; it was **the loop must never wait for a human**. A prompt binds the pipeline to a person. An
asynchronous setting change does not: nothing blocks, nothing is signalled, the executor is untouched, and
a cycle already in flight keeps the value it started with. What it costs on the hot path is one volatile
read, and the emitted result is only rebuilt when the setting actually changed.

A new setting goes through the **same type and schema check** as a literal, a stored binding or an
operator's first answer. A running graph is the last place that should be allowed to receive an ill-typed
value.

**A `select` is tunable too, and it picks rather than types.** Its runner publishes the collection it is
narrowing on every cycle, so the dashboard offers the *current* candidates — not the ones the process
started with. Choosing one stores the property named by `by`, exactly as the first-run picker does, so a
mid-run choice and a first-run choice write the same binding. The pipeline keeps running while the list is
open and the choice lands on the next cycle.

A `select` whose `criterion` comes from an **edge** is deliberately *not* offered: the edge would overwrite
any tuning on the very next cycle, and a control that silently does nothing is worse than no control. The
pre-pass marks those nodes and the activator skips registering them — the `value` node behind the edge is
the tunable instead.

**What is tunable is decided by when the value is consumed.** A threshold is consumed per frame, so
turning it mid-run means something. Which camera to open is consumed at activation — "changing" it means
closing a device and opening another, which is reconfiguration, not tuning, and is deliberately not
offered. That is the same distinction as the open `cam.device` question, seen from the other side.

Changes are persisted immediately, to the binding — so tomorrow's run starts where the operator left off.
A value with **no** binding is tunable but not persisted: its only durable home would be a literal in
`pipeline.json`, and rewriting that would make a per-machine tuning session edit the portable artifact.
The dashboard marks those with a `*`.

Terminal input has one hard limit worth stating: there is no mouse. Spectre.Console's live display gives
no mouse events, so navigation is keyboard-only. Click-to-edit belongs to the studio, not to the TUI.

## Binding store

Machine-local, outside the package: `.mvf/bindings.json`, next to the checkpoint directory.

That placement is the point. The same `pipeline.json` deploys to ten panel PCs; each binds to its own
camera; the pipeline file is byte-identical everywhere. Writing the selection back into `pipeline.json`
would make a versioned, portable artifact machine-specific.

## Validator rules

| code | when |
|---|---|
| `pipeline.node.invalid-value-type` | `type` is not a known value type |
| `pipeline.node.invalid-value-shape` | `shape` is neither `one` nor `list` |
| `pipeline.node.invalid-schema` | `schema` is not valid JSON Schema |
| `pipeline.node.literal-type-mismatch` | `literal` does not match `type`/`schema` |
| `pipeline.node.unresolvable-value` | no `literal`, `binding`, `default`, and no resolver configured |
| `pipeline.node.select-type-mismatch` | `criterion` element type ≠ `items` element type |
| `pipeline.node.select-invalid-mode` | `mode` is neither `one` nor `many` |

`unresolvable-value` fires when a node declares no `literal`, no `binding` and no `default` — with no
binding name there is nowhere for a resolver to even store an answer, so nothing could ever resolve it.
A node with only a `binding` is valid; whether that binding *has* a value is a per-machine fact and
belongs to the pre-pass, not to a validator that must give the same verdict on every machine.

An unknown `type` is carried through expansion verbatim (`control/value:decimal`) rather than thrown on,
so the failure arrives as a validator issue naming the node — the same split as `activationMode` and
`backpressure`, where the expander carries and the validator judges.

## Explicitly out of scope

`value` produces **one** value; it is not a form. No field groups, no layout, no conditional visibility.
The resolver renders *a type* — a string type gets a text entry, a list gets a picker — never a UI spec
carried in the graph. A pipeline needing five values declares five `value` nodes, which is honest and
still readable.

That holds up under load: `packages/value-demo` wires three of them into two `select`s, each value with
its own type, its own binding and its own consumer. Tuning one re-narrows only the `select` it feeds. A
single "settings" node holding all three would have to invent a way to address a field from an edge, and
the graph would stop describing where each value actually goes.

The line matters: the moment a node describes widgets, the graph stops being a dataflow description and
the typed-graph thesis dissolves.

## Worked example — the camera picker

```json
{
  "nodes": [
    { "id": "discover", "module": "mvf.camera-discovery",
      "config": { "protocol": "gige" } },

    { "id": "pickCam", "primitive": "select",
      "config": { "mode": "one", "binding": "camera", "by": "serial",
                  "prompt": "Select a camera" } },

    { "id": "cam", "module": "mvf.gige-camera" }
  ],
  "edges": [
    { "from": "discover.cameras", "to": "pickCam.items" },
    { "from": "pickCam.selected", "to": "cam.device" }
  ]
}
```

First run on a machine: the pre-pass runs discovery, the resolver shows the six cameras, the operator
picks one, the serial is stored in `.mvf/bindings.json`. Every run after that is silent. On a second panel
PC the same file binds to a different camera. Replacing the interactive choice with
`"where": { "serial": "ABC123" }` in config removes the operator entirely and changes nothing else.

Two parts of that are still ahead: the discovery module itself, and the pre-pass running it to fill the
picker. What works today is everything downstream of those — the binding resolves before cycle 0, and
`select` narrows the collection at run time:

```json
{
  "nodes": [
    { "id": "discover", "module": "some.discovery" },
    { "id": "pickCam", "primitive": "select",
      "config": { "mode": "one", "binding": "camera", "by": "serial" } }
  ],
  "edges": [ { "from": "discover.cameras", "to": "pickCam.items" } ]
}
```

With `camera` bound to `"DEF456"` — by hand, by `MVF_BINDING_CAMERA`, or by a prompt on the first run —
the pre-pass parks the criterion as a constant and `pickCam` emits that one camera record out of whatever
discovery published. `ValueAndSelectGraphTests` runs exactly this, including two machines binding the same
pipeline file to different cameras.

## Serial only, for now

Pipelined mode **rejects** a graph containing `value` or `select`, up front, the same way it already
rejects on-demand nodes and multi-producer ports. A `value` node has no inputs, so as a pipelined stage it
has no queue to read and nothing to pace it: whether it emits once and holds, or once per source cycle, is
a real decision and not one worth guessing at. Serial mode is the default and handles both primitives.

## Open

- **Names.** `value` and `select` are the working names. `value` is generic on purpose — it promises
  nothing about where the value comes from, which is right, but it is a plain word to reserve in the
  primitive namespace.
- **`cam.device` as an input port on a source node.** A source that takes a control input is unusual; it
  must consume it at activation, not per frame. The alternative is config templating
  (`"address": "${binding:camera.address}"`), which needs no new activation semantics but makes the graph
  less self-describing. Decide before building the camera side. *Nothing built so far depends on the
  answer* — the only consumer of a value today is `select.criterion`, which is an ordinary control input.
- **Store versus environment, when both have a value.** The store is read first, so a value bound once by
  an operator outranks a later environment variable on that machine. Transient answers keep that from
  being a trap in the common case (the store simply stays empty), but the ordering is inherited from the
  resolution order above and has not been argued on its own merits.
- **Candidates from a module, not just a `value`.** The picker is live: the pre-pass walks the edge into
  `select.items`, and when the producer is a list-shaped `value` it offers those elements as choices
  (topological order guarantees the collection is settled first). What is still missing is doing the same
  when the producer is a **module** — that means activating and running a discovery node before cycle 0,
  which is a real decision about pre-pass lifecycle, not a lookup. Everything downstream of the seam is
  done; only where the candidates come from changes.
- **Which discovery protocol first** — GigE Vision/GenICam, Cognex, or USB/UVC. This does not affect the
  primitives at all, but it is where the real work is.

## Where the code is

| piece | file |
|---|---|
| types + schema subset | [`src/core/Mvf.Graph/Values/`](../src/core/Mvf.Graph/Values/) |
| resolver + store seams | `IValueResolver`, `IValueBindingStore` in `Mvf.Abstractions` |
| primitives | `ValuePrimitiveNodeRunner`, `SelectPrimitiveNodeRunner` |
| pre-pass | [`BindingPrePass`](../src/engine/Mvf.Engine/Values/BindingPrePass.cs) |
| resolvers | `EnvironmentValueResolver`, `ChainedValueResolver`, `TerminalValueResolver` (CLI) |
| live tuning | [`LiveValueRegistry`](../src/core/Mvf.Graph/Values/LiveValueRegistry.cs), `PipelineDashboard` |

A demo package that needs no hardware: [`packages/value-demo`](../packages/value-demo/pipeline.json).
