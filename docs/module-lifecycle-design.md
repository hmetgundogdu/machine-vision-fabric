# Module Lifecycle — Design (standards-aligned)

> Status: **design agreed at the decision level** (2026-07-23, with the user); implementation scheduled
> as the **L-track** in `roadmap.md`. Companion to `data-plane-design.md`.

## The reframe (why this doc exists)
The trigger was "model lifecycle," but the real abstraction is **module readiness**. A module becoming
usable can mean **loading an ML model**, **loading another package/assembly**, **connecting to a device**
(camera/PLC), or just **finishing its own init**. These are the same problem — *"is this module ready to
do work yet, and how does the engine treat it while it is not?"* CLAUDE.md already makes lifecycle a
**first-class part of the node contract**, not an implementation detail. Today that contract is a lie:
`NodeActivationMode {Resident, OnDemand}` and a `activationMode` string exist but **nothing consumes
them** — every node is eagerly activated before cycle 0 and disposed at run end, so everything is
de-facto "resident," and there is no readiness signal, no warmup budget, no graceful drain.

This design makes the lifecycle **real, declared, observed, and enforced**, aligned to the dominant
industry standards rather than invented from scratch.

## Standards we align to (researched 2026-07-23)
| Standard | What we take from it |
|----------|----------------------|
| **Kubernetes probes** — startup / readiness / liveness | The three-signal split. A slow model load is a *startup* concern, not a *liveness* failure — you must not restart a module that is simply still warming up. Readiness gates work routing; liveness gates restart. |
| **OSGi bundle lifecycle** — INSTALLED→RESOLVED→STARTING→ACTIVE→STOPPING→…→UNINSTALLED | An explicit, framework-enforced **state machine** with `start`/`stop` activator hooks. You cannot jump straight to ACTIVE; transitions are ordered. |
| **systemd `sd_notify`** — `READY=1`, `STOPPING=1`, watchdog | Readiness is a **signal the process emits about itself**, not something the supervisor guesses. Maps onto our stdio control plane: a worker sends `ready` when warmup completes; `stopping` when draining. Handle the no-supervisor (dev/test) case gracefully. |
| **Triton model-control modes** — NONE / EXPLICIT / POLL, plus warm pools & lazy-load | The proven taxonomy for *loading* profiles: load-all-at-startup (resident), load-on-demand (on-demand/lazy), watch-and-reload (hot-reload). Warm pools hide cold-start for heavy instances. |

## MVF module lifecycle

### States (OSGi-style, mapped to our runtime)
```
Registered ──▶ Activating ──▶ Ready ⇄ Running ──▶ Draining ──▶ Stopped
                  │                                  ▲
                  └──(crash)──▶ Failed ──(restart+restore)──┘
```
- **Registered** — known from `module.json`, not yet activated. (OSGi INSTALLED/RESOLVED)
- **Activating** — warmup: load model / load package / connect device / init. (OSGi STARTING; K8s startup window)
- **Ready** — warmup complete, may accept frames. (OSGi ACTIVE; systemd `READY=1`; K8s readiness pass)
- **Running** — actively processing (a substate of Ready).
- **Draining** — graceful stop: finish in-flight, release external resources (camera/GPU/PLC/file). (OSGi STOPPING; systemd `STOPPING=1`)
- **Stopped** — disposed.
- **Failed → Restarting** — crashed; the supervisor restarts and restores from the last checkpoint. (K8s liveness fail → restart; ties into the existing `SupervisedWorker` + M2.5 recovery)

### Three health signals (Kubernetes taxonomy — the core of the contract)
1. **Startup / warmup done** — did activation finish? A heavy model load must be given a **startup budget**
   and must *not* be treated as a hang while inside it. Our activation already runs before cycle 0; we make
   its **completion an explicit signal** and **measure its duration** so a slow preload is visible, not silent.
2. **Readiness** — is the module ready to accept work *now*? A worker **signals `ready`** over stdio
   (sd_notify-style) rather than the engine assuming "activated == ready." Until ready, the engine does not
   route frames to it (the source stalls or drops per the backpressure policy).
3. **Liveness** — is it still alive? Already handled by `SupervisedWorker` crash detection + retry.

### Loading profiles (Triton taxonomy, generalized from models to modules)
Declared per module (default) and overridable per pipeline node:
- **`resident`** (≈ Triton NONE / always-on) — load at startup, keep warm for the whole run. **Default for
  `ai-model`, `camera/source`, `plc/control`.** This is "resident + preloaded" made honest.
- **`on-demand`** (≈ Triton EXPLICIT / lazy-load) — activate on first use; optionally unload after N idle
  cycles. **Default for a short helper process.**
- **`pooled` / warm-pool** (≈ warm pools) — a pre-warmed set of instances so restart/scale hides cold-start.
  **Default for a heavy external worker; override allowed.**
- **`hot-reload`** (≈ Triton POLL / package watch) — reload a module when its package changes. **Frontier, later.**

### The contract is declared and enforced (fixes today's dangling enum)
- `module.json` gains an optional **`lifecycle`** block: default profile + whether the module emits a
  readiness signal + an expected **warmup budget** (startup timeout).
- The pipeline node's `activationMode` **overrides** the module default.
- The engine **parses to `NodeActivationMode`, validates it (rejects unknown values), and acts on it** —
  the field stops being decorative.

## Milestone mapping → L-track (see `roadmap.md`)
- **L.1** — Lifecycle contract made real & observed (declare + validate + measure warmup). No behavior
  change for resident; the honest, low-risk first cut.
- **L.2** — Explicit readiness signal over stdio (sd_notify-style); startup-vs-liveness separation so a
  slow model load isn't mistaken for a hang.
- **L.3** — On-demand & idle-unload (Triton EXPLICIT / lazy-load) — needs a real short-helper node.
- **L.4** — Warm pools (pre-warmed instances hide cold-start / restart).
- **L.5** — Hot-reload / package watch (Triton POLL). Frontier.

## Honest scope note
Building a **real ONNX inference node** is a *separate, larger* item (needs a real model + hardware story)
and is **not** part of the L-track — the L-track defines the lifecycle *contract* that such a node (and a
package loader, and a device connector) will all plug into. Get the contract right first; heavy nodes
inherit it.

## Sources
- Kubernetes — [Configure Liveness, Readiness and Startup Probes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/), [Probes concepts](https://kubernetes.io/docs/concepts/workloads/pods/probes/)
- OSGi Core — [Life Cycle Layer](https://docs.osgi.org/specification/osgi.core/8.0.0/framework.lifecycle.html)
- systemd — [sd_notify(3)](https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html)
- NVIDIA Triton — [Model Management (control modes)](https://github.com/triton-inference-server/server/blob/main/docs/user_guide/model_management.md)
