# MVF management API v1 (#33)

How a client observes a running MVF runtime. Version 1 is **read-only**: a client can see the
graph, its live state, events and logs, and cannot change anything. Commands come with the
authorization model of part 2 ([ADR-0007](../docs/rust-runtime/adr-0007-runtime-management.md)).
Reference implementations: `rust/crates/mvf-management` (server and client), `mvf-rs manage` (CLI)
and `clients/mvf-manage-py` (a standard-library Python client).

## Endpoint

- Started by `mvf-rs run --manage`.
- **Unix:** a socket in the per-user runtime directory: `$MVF_RUNTIME_DIR`, else
  `$XDG_RUNTIME_DIR/mvf`, else `~/.mvf/run`.
  - The directory must be mode 0700 and owned by the user; anything else is refused. The socket
    is 0600.
  - The server also checks every connection's peer uid (`SO_PEERCRED` / `getpeereid`) and refuses
    any other user with `unauthorized`.
- **Windows:** a named pipe `\\.\pipe\mvf-…`.
  - Its DACL grants access to the current user and LocalSystem only. Remote clients are
    rejected, and the first instance is exclusive.
  - The registry file lives in `%LOCALAPPDATA%\mvf\run`.
- **Discovery:** each runtime writes `<name>.json` next to its endpoint with `endpoint`, `pid`,
  `pipeline`, `startedAtMs` and `api`. Both files are removed when the runtime stops.
- There is no network listener. Remote access is a separate, optional relay (part 2).

## Framing

One JSON object per line, UTF-8, in both directions. A request line is at most 64 KiB.

## Requests

| Request | Reply |
|---|---|
| `{"op":"hello","api":[1],"client":"name"}` (must come first) | `{"op":"hello","api":1,"runtime":{pid, version, pipeline, mode, startedAtMs, api}}` |
| `{"op":"snapshot"}` | `{"op":"snapshot","seq":N,"snapshot":{…}}` |
| `{"op":"ping"}` | `{"op":"pong"}` |
| `{"op":"subscribe","from":S}` | the stream below, starting at entry `S` |
| `{"op":"subscribe"}` | a snapshot, then the stream after it |

Errors are `{"op":"error","code":…,"message":…}`. The codes:

| Code | Meaning | Connection |
|---|---|---|
| `hello-first` | a request came before `hello` | closed |
| `api-unsupported` | none of the offered versions is spoken | closed |
| `malformed` | the line is not JSON | closed |
| `too-large` | the line is over 64 KiB | closed |
| `unknown-op` | the op is not defined | stays open |
| `unauthorized` | wrong OS user | closed |
| `busy` | more than 8 clients | closed |

## Snapshot

```json
{
  "api": 1,
  "runtime": {"pid": 4242, "version": "0.1.0", "pipeline": "inspection", "mode": "serial", "startedAtMs": 1764200000000, "api": 1},
  "status": "starting | running | paused | stopped",
  "end": {"reason": "source-exhausted", "node": "cam"},
  "cycle": 1200,
  "topology": {
    "pipeline": "inspection",
    "nodes": [{"id": "cam", "contract": "mvf.synthetic-source@1.0.0", "backend": "in-process", "parallelism": 1,
               "inputs": [], "outputs": [{"name": "frame", "type": "frame", "channel": "data"}], "liveFields": []}],
    "edges": {"data": [{"from": "cam.frame", "to": "sink.frame", "type": "frame"}], "control": []}
  },
  "nodes": {"cam": {"generation": 1, "executed": 1200, "skipped": 0, "lastSkip": null,
                    "latencyUs": {"p50": 3.2, "p95": 5.1, "p99": 9.0, "max": 41, "mean": 3.6},
                    "window": {"executed": 98, "meanUs": 3.4, "maxUs": 12, "perSecond": 980.0}}},
  "recentLogs": [], "recentEvents": [], "deferredPublications": 0
}
```

- **Topology:** contracts, backends, ports and edges. Config values are never included; they may
  hold credentials. `liveFields` names the editable fields only.
- **Node latency:** from a log-scale histogram, so percentiles are within about ±4.5 %.
  - `latencyUs` covers the whole run.
  - `window` covers the period since the previous delta (100 ms by default).
- **`recentLogs` and `recentEvents`:** the last 50 of each.

## Stream

```json
{"op":"entry","seq":17,"entry":{"kind":"delta","atMs":812.4,"cycle":96,"nodes":{"cam":{…}}}}
{"op":"entry","seq":18,"entry":{"kind":"event","event":"replacement","detail":{…}}}
{"op":"entry","seq":19,"entry":{"kind":"log","atMs":900.1,"node":"cam","generation":1,"level":"info","message":"…"}}
{"op":"entry","seq":20,"entry":{"kind":"dropped","logs":312}}
{"op":"entry","seq":21,"entry":{"kind":"status","status":"stopped","cycle":1200,"end":{…}}}
{"op":"heartbeat","seq":21}
{"op":"gap","from":3,"to":40}
{"op":"snapshot","seq":4130,"snapshot":{…}}
```

- **One sequence for everything,** strictly increasing by 1.
- **Entry kinds:**
  - `delta`: the nodes that changed, as absolute values, so applying a delta twice is harmless;
  - `event`: `replacement`, `recovery` or `live`, with the engine's own record;
  - `log`;
  - `dropped`: log lines refused by the telemetry budget;
  - `status`: `running`, `paused` or `stopped`, the last with the run's end reason.
- **`heartbeat`:** sent every second when nothing else is. It also detects a dead client.
- **Reconnect:** subscribe again with `from` = the last `seq` seen + 1. Nothing is lost or
  repeated while the server still holds those entries (4096 by default).
- **Gap:** when a client asks for entries the server no longer holds, or falls that far behind,
  it gets `gap`, then a fresh `snapshot`, then the stream after it.
- **End of the run:** the stream delivers the final `status` before the runtime closes the
  endpoint. The server waits up to 1 s for clients to catch up.

## Guarantees and budgets

- **The engine never waits for a client.** Node counters are kept on the engine thread and
  published at most every 100 ms with a non-blocking lock attempt. When a reader holds the lock,
  the counters wait for the next cycle; `deferredPublications` counts how often.
- **Log lines:** at most 200/s sustained, bursts of 1000. Excess is counted in `dropped`, not
  queued.
- **Clients:** at most 8. On Unix a write blocked for 2 s drops that client. On Windows a stuck
  client holds only its own thread.
- **Measured overhead:** about 15 ns per node event on the engine thread. A watching client
  receives about 20 KiB/s ([stage 24](../docs/rust-runtime/stages/24.md)).
