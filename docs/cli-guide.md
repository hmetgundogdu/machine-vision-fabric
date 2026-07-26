# MVF CLI — from zero to a running pipeline

The `mvf` CLI is the edge runtime: it discovers modules, validates a pipeline graph, runs it, and
draws a **live dashboard** while it executes. This guide walks the whole path A → Z — install,
inspect, validate, run, **loop**, drive the dashboard live, and read back a finished run.

Everything here is **hardware-free**: the demo packages use simulator sources, so you can follow
along with nothing plugged in.

> In the examples below, a downloaded release binary is just `mvf`. From a source checkout, replace
> `mvf` with `dotnet run --project src/cli/Mvf.Cli --`.

---

## A · Get the CLI

**Download** a single self-contained binary for your OS from the
[latest release](https://github.com/hmetgundogdu/machine-vision-fabric/releases/latest) — no .NET
required:

```bash
chmod +x mvf-cli-linux-x64
mv mvf-cli-linux-x64 mvf
./mvf --help
```

**Or build from source** (needs the .NET SDK pinned in `global.json`, .NET 10):

```bash
dotnet build Mvf.slnx -c Release
dotnet run --project src/cli/Mvf.Cli -- --help
```

The commands, at a glance:

| Command | What it does |
|---|---|
| `packages` | list runnable pipeline packages |
| `modules` | list the integration modules discovered on disk |
| `validate-pipeline` | type/schema-check a graph without running it |
| `execute-graph` | **run** a pipeline (live dashboard by default) |
| `sessions` | list finished runs and their output sessions |
| `inspect-session` | print the manifest of one finished run |
| `schemas` | export the JSON Schemas for pipeline/config authoring |

---

## B · See what's runnable

```bash
mvf packages     # every folder under packages/ that has a pipeline.json
mvf modules      # every module.json the runtime can launch (.NET, Python, C++)
```

A **package** is just a folder with a `pipeline.json` (plus any assets it needs — frames, models,
scripts). A **module** is one reusable node the graph references by `id`. `packages` and `modules`
are how you find those ids before wiring a graph.

---

## C · Validate before you run

Validation is static — it checks the graph is a legal, strictly-typed DAG (every edge connects a
real output port to a compatible input port, every config matches its schema) **without** launching
anything:

```bash
mvf validate-pipeline --path packages/inspection-demo/pipeline.json
```

Fix any reported error here and the run will start clean. This is the fast inner loop while
authoring a graph.

---

## D · Run it — the live dashboard

```bash
mvf execute-graph --package packages/inspection-demo
```

You get the live TUI dashboard (the screenshot in the README): color-coded node boxes with their
config and per-node stats, the executing node highlighted, and a **rolling log panel** at the
bottom — run id, every node execution with its cycle index and duration, and any recovery events.

For plain, non-interactive output (CI, logs, a headless box) add `--no-tui`:

```bash
mvf execute-graph --package packages/inspection-demo --no-tui --max-cycles 3
```

---

## E · The loop — how a pipeline *keeps* running

This is the part that trips people up, so it gets its own section.

A pipeline's data graph is a **strict DAG** — frames only ever flow forward, `cam → … → sink`, and
no data edge is allowed to cycle back. So by itself the graph describes **one pass**: pull a frame,
push it through, write it out. Run a bare DAG and it does exactly that and stops.

To make it *iterate* — replay a folder, run forever off a live camera, or stop after N cycles — you
add one node: the **`loop` primitive**. The loop is the graph's single **iteration authority**. It
owns two things and nothing else:

- **When to stop** (`mode`): `until-exhausted` (default — run until the source stops producing),
  `forever` (a finite source is rewound and replayed; a live camera just never exhausts), or
  `count` (stop after N cycles).
- **Running state**: whole-graph pause/resume (SPACE in the dashboard). Pause is *not* cancel — the
  process stays alive and workers stay warm; resume continues where it left off.

You wire it with **two edits** to your `pipeline.json`:

1. **Add a loop node.** It is portless — its behaviour is entirely in `mode`:

   ```json
   { "id": "cycle", "primitive": "loop", "config": { "mode": "forever" } }
   ```

2. **Close the tail back to it** with a plain **node-level** edge — by id, no ports (it moves no
   value, so it needs no typed port). Your forward data edges stay typed, port to port:

   ```json
   "edges": [
     { "from": "cam.frame",  "to": "save.frame" },   // data — typed, forward, DAG
     { "from": "save",       "to": "cycle" }          // loop close — by id, no port
   ]
   ```

That's the whole model: **data flows forward as a DAG; the loop closes by id and owns iteration.**
There is no `loop:true` flag on the source anymore — iteration lives in exactly one place.
`packages/loop-demo/pipeline.json` is the minimal, hardware-free example; `inspection-demo` is a
13-node one. Both use `mode: forever`.

Cap any run — even a `forever` one — from the CLI:

```bash
mvf execute-graph --package packages/loop-demo --max-cycles 200
```

---

## E2 · When a node fails — staying alive

A `loop` keeps a pipeline *iterating*; this keeps it *alive* when the world misbehaves. The failure
policy is the **runtime's** to set, not the module's: a module throws and says what broke (a camera
request timing out, a sink losing its connection), and you decide what happens next.

By default the runtime already does the sensible thing per role — a **source** that fails ends the
run (a dead camera is not a clean, empty success), while a **mid-graph** node's cycle is skipped and
the run carries on. `onError` lets any node opt into **restart** instead.

There are two actions — fall through to that default, or **restart the node** — and one number that
says how hard to try. Set the source default with `--on-source-error`, or put `onError` on **any**
node (a source, a sink, a classifier):

| Mode | On a source failure |
|---|---|
| `restart` *(default)* | **Restart the node** — dispose it and bring it back from scratch (a fresh session), back off, and read again. `--source-restart-limit` caps how many restarts before the run fails; **`0` (the default) means forever**, so the run rides out an outage and resumes the moment the source comes back. One verb whether the source is a camera, a file, or a simulator. |
| `fail` | End the run at once with the source's error. The strict, honest fast-fail — good for CI. |

```bash
mvf execute-graph --package packages/loop-demo                          # restart forever (default)
mvf execute-graph --path my.json --on-source-error restart --source-restart-limit 6
mvf execute-graph --path my.json --on-source-error fail                 # strict
```

Per node, in `pipeline.json` (overrides the default) — a bare string is `restart`-forever, or spell out
the knobs. Works on a source, and equally on a mid-graph node that would otherwise just skip:

```json
{ "id": "cam", "moduleId": "ivp.cognex-hmi-camera",
  "config": { "onError": { "mode": "restart", "limit": 0, "backoffMs": 1000, "maxBackoffMs": 30000 } } }

{ "id": "save", "moduleId": "mvf.dataset-writer",
  "config": { "onError": "restart" } }   // a sink that reconnects instead of dropping the cycle
```

Three things worth knowing:

- Only the **read path** is made resilient. A source that is *entirely absent at startup* still fails
  fast, so a wrong address is reported immediately — not hidden behind an endless retry.
- A **hard restart** rebuilds the node from scratch (a fresh session), so it recovers even when the
  connection is not just stalled but dead.
- A **bounded** restart that runs out of its limit **still fails the run** — a source that never
  recovers is never mistaken for a clean, empty success. Restart notices show live in the node's log.

---

## F · Drive it live

While a looping run is on screen, the dashboard is interactive:

| Key | Action |
|---|---|
| `SPACE` | pause / resume the whole graph (state kept, workers warm) |
| `←` / `→` (or `Tab`) | walk between nodes; `↑` / `↓` move within a layer |
| `ENTER` | open the selected node's detail page — config, live stats, and its own log |
| type + `ENTER` | on the detail page, edit a live-editable field (see below) |
| `Esc` / `q` / `←` | close the detail page, back to the graph |
| `Ctrl+C` | stop the run (or it stops on its own at `--max-cycles`) |

Two kinds of value can be edited **while the run continues**, both type- and schema-checked and
remembered for next time:

- A **`value` node** (e.g. a brightness `threshold`) — the edit lands on the **next cycle**, no
  restart.
- A **module binding** (e.g. `cam.frameIntervalMs`) — the node **re-activates** with the new value
  at the next cycle boundary (a source restarts at frame 0). A node declares which fields are
  live-editable in a `bindings` map.

---

## G · Read back a finished run

A run writes an output **session** (frames, decisions, a manifest). List them and inspect one:

```bash
mvf sessions
mvf inspect-session --path <session-folder-or-session.json>
```

`inspect-session` prints the run manifest — what ran, how many cycles, what each sink wrote — so a
finished run is auditable after the fact, not just live.

---

## H · `execute-graph` options reference

```
execute-graph
  --package <path>            a package folder (contains pipeline.json)   ── or ──
  --path <pipeline.json>      a single pipeline file
  --integrations-root <path>  where to discover modules (defaults per layout)
  --max-cycles <n>            stop after n cycles (caps even a `forever` loop)
  --mode serial|pipelined     execution mode (a `loop`/`value`/`select` graph is serial-only)
  --checkpoint-every <n>      write a resumable checkpoint every n cycles
  --resume-dir <path>         resume a run from its last checkpoint
  --backpressure stall|drop   what a fast source does when a stage falls behind
  --on-source-error <mode>    on a source failure: restart (default) or fail
  --source-restart-limit <n>  restarts before the run fails (0 = forever, default)
  --source-backoff-ms <n>     first backoff before a restart, doubled each attempt (default 500)
  --queue <n>                 inter-stage queue depth (pipelined mode)
  --arena-slots <n>           override the shared-memory arena slot count
  --no-tui                    plain output instead of the live dashboard
  --no-prompt                 never ask an operator for an unresolved binding; fail instead
```

For the design behind these, see [`loop-and-running-state-design.md`](loop-and-running-state-design.md)
(loop + pause), [`value-and-select-design.md`](value-and-select-design.md) (live values / routing),
and [`data-plane-design.md`](data-plane-design.md) (the shared-memory arena, checkpoint/resume).

---

## Z · The whole path, in one block

```bash
mvf packages                                                   # A → what can I run?
mvf validate-pipeline --path packages/loop-demo/pipeline.json  # C → is my graph legal?
mvf execute-graph --package packages/loop-demo --max-cycles 200 # D+E → run it, looping
#   ↑ SPACE pauses · ←/→ walk nodes · ENTER opens a node · type to edit a live value
mvf sessions                                                   # G → what did it write?
```

New to authoring the *nodes* themselves? See the **Write a module** walkthrough in the
[README](../README.md) and the [SDK Quickstart](sdk-quickstart.md).
