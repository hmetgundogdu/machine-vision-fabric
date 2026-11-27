# The `mvf` TUI (#35)

An interactive terminal dashboard for a running pipeline: the live graph, node details, logs and
events, live edits and previews.

- **A management client, not a second runtime.** It reads the same subscription as
  `mvf manage watch` and sends the same commands as `mvf manage command`. So the roles,
  validation and audit of [management](../../protocol/management-v1.md) apply to it.
- **No network service.** It talks to the local endpoint only; no Docker either.

```text
mvf run pipeline.yaml --tui                  # run with the dashboard on top; q stops the run
mvf run pipeline.yaml --manage               # or: run headless...
mvf manage tui                               # ...and attach from another terminal; q only detaches
mvf manage tui --endpoint <socket|pipe>      # when several runtimes are registered
mvf manage tui --print 120x36                # one frame as plain text (no terminal needed)
```

The `inspection-demo` (`rust/examples/inspection-demo`) running under `mvf manage tui`, captured
with `--print 160x40` and cut to 118 columns:

```text
MVF inspection-demo  >> RUNNING   cyc:147  4.0/s  t:37.15s  mode:serial  pid:91945
sel:cam  follow:on   * src * compute * classify * flow * sink * value  --- data  === control

                                                                                 +--------------------+
                                                                             +-->|+ invert            |-frame--------+
 +--------------------+                                                      |   |W py.invert-transfo~|              |
 |- cycle             |                                                      |   |x147 232us          |              +
 |P mvf.primitive.loo~|                                                      |   +--------------------+
 |waiting             |                                                      |
 +--------------------+                                                      |   +--------------------+
                                                                         +------>|+ bright            |-class------+
 #====================#                  +--------------------+          |   |   |W py.brightness-cla~|            |
 #| cam *             #-frame----------->|+ fork              |-toCount----+-+   |x147 152us          |            +--
 #B mvf.folder-source~#                  |P mvf.primitive.for~|-toSamp-+-+ |     +--------------------+          +----
 #x147 252.6ms        #                  |x147 7us            |-toRo-+ |   |                                     |
 #====================#                  +--------------------+      +-|-----------------------------------------+
                                                                       |   |
 +--------------------+                                                |   |     +--------------------+
 |+ thr *             |                                                |   +---->|+ count             |-class---------
 |P mvf.primitive.val~|                                                |         |W py.frame-counter@~|        +------
 |x147 2us            |                                                |         |x147 136us          |        |
 +--------------------+                                                |         +--------------------+        |
                                                                       |                                       |
                                                                       +---------------------------------------+
```

`cam` is selected (`#` border) and running: its spinner shows, and its 252 ms per cycle is its
250 ms pacing. A real terminal adds colour, a breathing border on the running node, an afterglow
on the node that just ran, and a comet along each data edge.

## What it shows

- **Header:**
  - pipeline and status (`RUNNING`, `PAUSED`, `DONE`, `FAILED`, `DISCONNECTED`);
  - completed cycles and cycles per second, run time, executor mode;
  - failures and restarts over all nodes, observation gaps recovered, runtime pid.
- **Graph:** nodes in layers from left to right (each in the layer of its longest path from a
  source), as in the legacy dashboard.
  - **Node box:**
    - a status icon: `-` not run yet, a spinner while running, `+` has run, `o` source
      exhausted, `x` failed;
    - the node id, and `*` when it has live-editable fields;
    - a hosting badge: `P` primitive, `B` builtin or in-process, `W` worker, `C` container, `G`
      GPU;
    - the active contract;
    - executions and p50 latency, then `!` failures, `r` restarts, `g` generation after a
      replacement, `|n` instances running.
  - **Edges:** data edges are `-`, control edges are `=`, each with its source port. An edge that
    skips layers passes through its own row instead of crossing boxes.
  - **A large graph** scrolls: the view keeps the selected node in sight, and `< > ^ v` at the
    edges say there is more.
- **Events and logs:**
  - replacements (promoted, rolled back, rejected);
  - recovery (restarted, skipped, dropped, checkpointed);
  - live edits, pauses, the end of the run;
  - module logs, with the runtime's dropped-log count.
- **Node details** (`enter`):
  - **Identity:** contract, backend and generation, all of them after a live replacement.
  - **Ports:** typed, with their channel and what each one is wired to.
  - **Editable fields:** type, schema bounds or choices, and whether an edit is persisted (a
    binding key) or lasts for this run only.
  - **Stats:**
    - status: active, idle, exhausted, failed;
    - executions, skips, failures, restarts, drops, the last skip reason;
    - latency p50/p95/p99/mean/max and an ASCII sparkline of recent windows.
  - **This node's events and logs**, and its **preview** (`p`).

## Keys

| Key | Graph | Details |
|---|---|---|
| `<-` `->`, `tab` | previous / next node in reading order | |
| up / down | node above / below in the layer | choose an editable field |
| shift+arrows, `h j k l` | pan the graph | |
| `f` | follow the running node on / off | |
| `enter` | node details | edit the field |
| `e` | | edit the field |
| `p` | | preview on / off |
| `esc` | | back to the graph |
| `space` | pause / resume (needs a `loop`) | same |
| `?` | help | help |
| `q`, `ctrl+c` | quit: `run --tui` stops the run, `manage tui` detaches | same |

- **Follow:** on by default. Moving by hand turns it off, and `f` turns it back on.
- **Edits:**
  - typed text is checked against the field's type and schema before it is sent;
  - it becomes `configure` (a module binding) or `set-value` (a `value` node);
  - the reply says `applied` (with `persisted` when the binding store kept it), `rejected` with
    the engine's reason, `pending`, or `refused` (for example a role without commands).

## Behaviour

- **It never holds the run up.** The runtime publishes without waiting for clients. Commands
  and previews use their own threads and connections. A frozen or crashed TUI therefore costs the
  pipeline nothing. In `mvf run --tui` a TUI crash restores the terminal and the run goes on.
- **"Running now" is measured, not guessed.**
  - The engine counts each node's executions in progress with relaxed atomics, at no cost to the
    hot path.
  - The feed samples the counts every 100 ms on its own thread.
  - So a slow node shows as running for as long as it actually runs.
- **Reconnects:**
  - after a dropped connection it resubscribes from the last sequence seen, with nothing lost
    or repeated;
  - after a gap it reloads from the fresh snapshot;
  - a restarted runtime is detected and loaded from a new snapshot.
- **Bounded memory:**
  - 300 log lines overall, 30 per node;
  - 200 events;
  - 120 latency samples per node;
  - only the latest preview.
- **Terminals:**
  - **Resize** redraws at the new size; below 40×12 it says the terminal is too small.
  - **No terminal:** without an interactive stdin and stdout, `manage tui` and `run --tui` refuse
    with exit 2 and point to `manage watch` / `status --json`.
  - **Closing the terminal** ends the TUI process. For `manage tui` the runtime is unaffected.
    For `run --tui` it is the run's own terminal, so prefer `manage tui` beside a headless
    runtime in production.
- **ASCII only.** Every glyph is one ASCII cell: borders, icons, shafts, spinner, sparkline,
  preview ramp. Panel-PC and Windows console fonts cannot make the screen drift (the lesson of
  the legacy `Glyphs` table).
- **Escape sequences are stripped:** control characters in log lines and events are replaced
  before display.
- **Previews** (`p` in details, needs `--preview NODE` on the runtime and the `previewer` role):
  - the runtime's capped PNG previews are shown, in 24-bit colour where the terminal says it
    supports it (`COLORTERM`, Windows Terminal), otherwise as an ASCII grey ramp;
  - a BMP-encoded blob (the Rotary analyzer's output) is decoded by the runtime, and the preview
    says so;
  - any other payload is named, never drawn: a PNG/JPEG-encoded blob, or a blob without a known
    image signature.

## Differences from the legacy .NET TUI

| | Legacy (`mvf-cli execute-graph`) | `mvf` |
|---|---|---|
| Process | the dashboard was the runtime | a client: `run --tui` runs both, `manage tui` attaches to a running one, and several can watch |
| Look | Spectre.Console panels, layered graph, category colours, ASCII glyphs | the same picture; long edges get their own row, crossings are reduced, each edge has a track |
| Running node | the engine's current node | sampled execution counts, which also covers stage-parallel runs |
| Config in the box | the config summary | not shown: management never exposes config values; editable fields are named and typed in details |
| Editing | typed in the dashboard | the same, through `configure` / `set-value`, with the engine's verdict and the audit |
| Worker CPU / memory, source receive split | per node | not yet in management, so not shown |
| Egress view (`e`) | yes | no: egress is not ported |
| Previews | none in the terminal | in the terminal (`p`); Display Fabric windows are separate (`mvf-display-host`) |

## Limits

- **Current values are not shown.** The values of editable fields never leave the runtime
  through management (ADR-0007). The TUI shows what it set itself in this session.
- **Shared attachment rows:** a node with more than three edges on one side shares attachment
  rows between them.
- **Not real terminals:** the Windows check is `manage tui --print` against a live runtime over
  the named pipe, plus the rendering tests. No interactive Windows Terminal session was driven
  in CI. Linux and macOS run a pseudo-terminal smoke test through `script`.
- **No side-by-side capture with the legacy TUI.** The look was ported from its source; the
  legacy runtime is archived.
