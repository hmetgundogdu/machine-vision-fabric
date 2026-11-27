# MVF management API v1 (#33)

How a client observes and commands a running MVF runtime: the graph, its live state, events and
logs, and authorized commands that go through the engine's own validation
([ADR-0007](../docs/rust-runtime/adr-0007-runtime-management.md)).

Reference implementations:
- `rust/crates/mvf-management`: the server and client;
- `mvf manage`: the local CLI;
- `rust/crates/mvf-remote`: the TLS relay and remote client;
- `clients/mvf-manage-py`: a Python client using only the standard library, local or over TLS.

## Endpoint

- Started by `mvf run --manage`.
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
- There is no network listener in the runtime. Remote access goes through the separate relay
  (see [Remote](#remote-the-relay)).

## Framing

One JSON object per line, UTF-8, in both directions. A request line is at most 64 KiB.

## Requests

| Request | Reply |
|---|---|
| `{"op":"hello","api":[1],"client":"name"}` (must come first) | `{"op":"hello","api":1,"runtime":{pid, version, pipeline, mode, startedAtMs, api},"principal":"local","role":"operator","features":["commands"]}` |
| `{"op":"command","id":"k-1","command":"…","args":{…},"deadlineMs":5000}` | see [Commands](#commands) |
| `{"op":"preview","node":"cam","fps":1}` | a stream of previews, see [Previews](#previews) |
| `{"op":"upload",…}`, `{"op":"upload-status",…}` | see [Uploads](#uploads) |
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
| `invalid` | `onBehalfOf` names no valid principal or role | closed |
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
               "inputs": [], "outputs": [{"name": "frame", "type": "frame", "channel": "data"}], "liveFields": ["intervalMs"],
               "live": [{"field": "intervalMs", "command": "configure", "type": "int", "shape": "one",
                         "schema": {"minimum": 0}, "binding": "cam.interval"}]}],
    "edges": {"data": [{"from": "cam.frame", "to": "sink.frame", "type": "frame"}], "control": []}
  },
  "nodes": {"cam": {"generation": 1, "executed": 1200, "skipped": 0, "failures": 0, "restarts": 0, "dropped": 0, "lastSkip": null,
                    "latencyUs": {"p50": 3.2, "p95": 5.1, "p99": 9.0, "max": 41, "mean": 3.6},
                    "window": {"executed": 98, "meanUs": 3.4, "maxUs": 12, "perSecond": 980.0}}},
  "activity": {"cam": 1},
  "recentLogs": [], "recentEvents": [], "deferredPublications": 0
}
```

- **Topology:** contracts, backends, ports and edges. Config values are never included; they may
  hold credentials. `liveFields` names the editable fields only.
- **Node latency:** from a log-scale histogram, so percentiles are within about ±4.5 %.
  - `latencyUs` covers the whole run.
  - `window` covers the period since the previous delta (100 ms by default).
- **`recentLogs` and `recentEvents`:** the last 50 of each.
- **`live` (#35):** how each editable field is edited:
  - `command` is `configure` (a module binding) or `set-value` (a `value` node);
  - `type` is `string`, `int`, `number`, `bool` or `json`, and `shape` is `one` or `list`;
  - `schema` (bounds, `enum` choices) and `binding` (the binding-store key that persists an
    edit) appear when declared.
  - The current value is never included.
- **Per-node counters:**
  - `failures`: failed executions the recovery policy handled (restarted, skipped, or out of
    restarts);
  - `restarts`;
  - `dropped`: cycles dropped by backpressure.
  - After a replacement or rollback, `contract` and `backend` name the now-active generation.
- **`activity` (#35):** executions in progress per node. The engine counts them with relaxed
  atomics, and the feed samples them every 100 ms on its own thread, so this is what was running
  at that instant (it is never inferred from counters). After the run every node is 0.

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
  - `activity`: `{"running": {"slow": 1, "cam": 0}}`, the nodes whose count of executions in
    progress changed, as absolute values;
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

## Sessions and roles

- **Roles:**
  - `observer`: snapshot, subscribe, ping;
  - `previewer`: also previews;
  - `operator`: also commands and uploads.
- **Line limit:** after an operator's `hello`, a request line may be up to 1 MiB (upload
  chunks); for everyone else it stays 64 KiB.
- The endpoint's owner is an `operator`, as principal `local`.
- **`onBehalfOf`:** a client running as the owner (the relay) may speak for someone else:
  `{"op":"hello","api":[1],"onBehalfOf":{"principal":"alice","role":"observer","via":"…"}}`.
  The session then has that principal and role, never more than `operator`. An unknown role is
  refused (`invalid`).
- **`features`** lists `commands` only when this runtime takes commands and the role allows
  them.

## Commands

```json
{"op":"command","id":"ops-17","command":"set-value","args":{"node":"thr","value":41},"deadlineMs":5000}
{"op":"command","id":"ops-17","command":"set-value","status":"applied","afterSeq":812,"event":{"kind":"value-set","node":"thr","cycle":96}}
```

| `command` | `args` | Applied when the engine reports |
|---|---|---|
| `stop` | — | status `stopped` (a clean stop at the next cycle boundary) |
| `pause` / `resume` | `forMs` (pause) | live `paused` / `resumed` (needs a `loop`) |
| `set-value` | `node`, `value` | live `value-set` |
| `configure` | `node`, `field`, `value` | live `reconfigured` or `reopened` |
| `replace` | `node` and one of `builtin`, `module` or `contract` (inside the package); `config`, `probationCycles`, `storage` | replacement `promoted` |
| `rollback` / `commit` | `node` | replacement `rolled-back` / `committed` |
| `stage` | `package`, `files: {path: digest}`, `signature: {key, sig}` when signers are trusted | the package is staged (no engine event); the reply carries `staged.contract` for `replace` and `staged.signedBy` |

- **Validation:** every command except `stop` becomes one update-plan step and is validated
  exactly as in `mvf run --updates`. The engine applies it at a cycle boundary. A contract path
  must stay inside the package: no absolute paths, no `..`.
- **`status`:**

  | Status | Meaning |
  |---|---|
  | `applied` | the engine reported the effect; `event` is its record |
  | `rejected` | the engine refused it; nothing changed |
  | `pending` | no outcome before `deadlineMs` (default 5000, at most 60000); follow the stream after `afterSeq` |
  | `refused` | not sent: `forbidden` (role), `unknown-command`, `invalid` (arguments), `id-required` or `unavailable` |

- **Idempotency:** `id` is required, 1–128 characters `[A-Za-z0-9-_.:]`. A repeated `id` from the
  same principal is never sent again; the first reply comes back with `"duplicate": true`. The
  last 4096 ids are remembered.
- **Audit:** every command, refused ones included, is appended to the runtime's audit log as one
  JSON line: principal, role, via, id, command, args, status, event. The default log is
  `<pipeline dir>/.mvf/audit.jsonl` (mode 0600), or the file given by `--audit`. Refused
  connections are recorded there too.
- **Shutdown:** replies to commands in flight are delivered before the endpoint closes. A `stop`
  therefore always gets its answer.

## Previews

Opt-in, capped, audited and never retained.

- **Opt-in:** a node can be previewed only if the runtime was started with `--preview NODE`.
  Otherwise the reply is `not-enabled`. A role below `previewer` gets `forbidden`.
- **The stream:**

  ```json
  {"op":"preview","node":"cam","seq":12,"cycle":480,"format":"png","width":160,"height":120,"sourceWidth":1280,"sourceHeight":960,"renderUs":698,"data":"iVBORw0K…"}
  ```

  Interleaved with `{"op":"heartbeat","previewsSkipped":n}` while there is nothing new.
- **Caps** (defaults):
  - at most 2 samples per second per node, and fewer if the client asks for less (`fps`);
  - at most 160 × 120 pixels, downscaled nearest-neighbour;
  - at most 256 KiB/s per viewer. A preview that does not fit the viewer's budget is skipped,
    never queued.
- **Format:** 8-bit grey, RGB or RGBA becomes PNG, uncompressed deflate, base64-encoded.
- **Encoded blobs (#35):** a byte blob is previewed only when its content is an image this
  runtime decodes:
  - **BMP** (uncompressed 8-bit palette, 24-bit, or 32-bit with standard masks), with every
    offset bounds-checked and only the downscaled pixels sampled. It becomes a PNG preview with
    `"encoding":"bmp"`.
  - **PNG- and JPEG-encoded blobs** get `"format":"none"` and a reason that names them.
  - **Any other blob** gets `"format":"none"` with "a blob without a known image signature". It is
    never shown as raw pixels.
- **Anything else** gets `"format":"none"` with a `reason`.
- **Sampling:** the engine hands over a shared lease through a single-slot queue and never
  waits. The preview thread drops the lease as soon as it has downscaled.
- **Retention and audit:** only the latest preview per node is kept in memory. Previews are not
  in the ring, snapshots, the audit log or on disk. Opening a preview is audited
  (`preview-opened`: principal, node, fps).
- **Executor:** both. In the pipelined executor a mid-graph node's preview is the output of its
  next execution after the sample was asked for, so its cycle may trail the source's by the
  queue depth. Every preview carries the cycle it shows.

## Uploads

A prepared node package (a directory with `node.yaml` and its files) goes to
`<pipeline dir>/.mvf/staged`. Uploads are operator-only and enabled with `--allow-uploads`.

- **A chunk:**

  ```json
  {"op":"upload","package":"inspect-v2","path":"model/weights.onnx","size":734003,"digest":"sha256:…","offset":524288,"data":"<base64>"}
  ```

  Replies, `status` by case:

  | `status` | When |
  |---|---|
  | `partial` | with `received` |
  | `complete` | the last byte arrived and the SHA-256 matched |
  | `rejected` | `digest-mismatch`; the file is discarded |
  | `refused` | `invalid` name, `offset-mismatch` (with `received`), `too-large` or `conflict` |

  A chunk the runtime already has is acknowledged with `"duplicate": true` and not written
  twice.
- **Resume:** `{"op":"upload-status","package":…,"path":…}` gives `received` and `complete`, so a
  client continues after a dropped connection or a runtime restart. A file of an already staged
  package reports `complete`.
- **Names:**
  - a package is 1–64 characters `[A-Za-z0-9-_.]` and does not start with a dot;
  - a path has up to 4 such segments: no `..`, no absolute paths, nothing hidden.
- **Limits:** a file is at most 256 MiB, a package at most 1 GiB and 64 files.
- **Staging:** the `stage` command checks every listed file's digest again, and that no file is
  missing or extra. It writes `manifest.json` and renames the package into place atomically.
  - Staging the same manifest again is a no-op (`alreadyStaged`).
  - Other contents under the same name are refused (`exists`): a staged package is immutable,
    so a new version gets a new name.
- **Activation:** an ordinary `replace` with `"contract": ".mvf/staged/<package>/node.yaml"`, and
  `rollback` to undo it.
- **Audit:** completed and rejected files are audited (`upload`), and so are `stage`, `replace`
  and `rollback`.
- **Signatures** (when the runtime is started with `--trust-signer KEY`, repeatable):
  - `stage` requires `signature: {"key": "ed25519:<hex>", "sig": "<base64>"}`, made by a trusted
    key over the package name and every file's digest:

    ```text
    mvf-package-signature-v1
    package <name>
    file <path> sha256:<hex>      (one line per file, sorted by path)
    ```

  - Refusal codes: `unsigned`, `untrusted-signer`, and `bad-signature` (the files or the name
    differ from what was signed, or the signature is malformed).
  - The signature is kept as `signature.json` beside the staged package's `manifest.json`.
  - Without trusted signers, packages stage unsigned: trust then rests on who may upload.
  - `mvf-remote sign keygen` and `mvf-remote sign package` make keys and signatures;
    `--signature FILE` on `upload` sends one.

## Remote: the relay

`mvf-remote serve` runs as the runtime's user, next to it, and listens only on the address it is
given.

- **Protocol:** the same JSON lines over **TLS 1.3 with a client certificate**. The relay checks
  each connection in turn:
  1. The certificate must chain to the relay's CA and be within its validity dates.
  2. Its SHA-256 fingerprint must be in `access.json` and not revoked. The file is re-read for
     every connection.
  3. The listed name and role become `onBehalfOf` on the relay's own connection to the local
     endpoint.
- **Commands, uploads and previews:** the relay refuses an op the role does not allow
  (`forbidden`) before it reaches the runtime, and the runtime would refuse it too. `preview`
  needs a previewer and streams like `subscribe`. `upload` and `upload-status` need an operator,
  whose request lines may be 1 MiB.
- **Audit:** the runtime audits commands under the remote principal, `via` the relay and the
  peer address. The relay keeps its own log of admitted and refused connections and forbidden
  requests.
- **Limits:** 8 clients, a 5 s handshake, a 5 min idle timeout, a 5 s write timeout, requests of
  at most 64 KiB.
- **Certificates:** `mvf-remote pki init|issue|revoke` manages a small private CA (ECDSA P-256).

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
