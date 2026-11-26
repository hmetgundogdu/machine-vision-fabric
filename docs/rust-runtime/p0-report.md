# P0 report — integrated runtime, benchmark gates and verdicts (#17–#20)

Machine-readable: [gate-b-report.json](gate-b-report.json). Decision record:
[ADR-0002](adr-0002-p0-runtime.md). Thresholds were fixed before the final runs:
[p0-thresholds.md](p0-thresholds.md) (commit `15d832c`).

## Verdicts

| Gate | Verdict | Why |
|---|---|---|
| **P0.1** Pure Rust graph execution | **PASS** (re-evaluated after #24 and #25, [stages 11–13](stages/11.md)) | No CLR; strict typed polyglot graph; separate data/control edges; cycle semantics; `stall`/`drop` backpressure; epoch checkpoint/restore with durable resume (incl. a SIGKILLed engine) and worker state recovery; restart policy; graph primitives and the `loop`; the pipelined executor with joins, per-node parallelism and epoch barriers. The .NET demo packages produce byte-identical outputs on their Rust twins, serial and pipelined. *First evaluation (before #24): FAIL (incomplete).* |
| **P0.2** Daemonless OCI execution | **PASS on Linux** · Windows **BLOCKED** | Real OCI images run through `runc` with no Docker daemon or host Python; digest-verified import; default-deny sandbox; lifecycle; typed graph node; Docker interop both ways. Windows needs WSL2/HCS, not implemented. |
| **P0.3** CPU and GPU zero-copy data plane | **PASS** (CPU everywhere; GPU on Apple M4/Metal) | `copiesIn = copiesOut = 0` on the shared-memory path; GPU chain with one upload and a 256-byte readback per frame, device hand-offs by handle. GPU sharing is in-process only (no cross-process/OCI GPU buffers). |
| **P0.4** Live node replacement | **PASS** | Prepared while running, atomic cut-over at a cycle boundary, probation, rollback with the failed cycle replayed; in-process, Python worker and OCI candidates; lossless (every cycle exactly once), no duplicate PLC commit. |
| **All four on one host** (#20 criterion 1) | **BLOCKED** | No available machine offers both a Linux OCI host and a GPU: the Mac has the GPU but cannot run Linux OCI images natively; the Linux VM runs OCI but exposes no GPU. Two integrated runs cover P0.1/P0.3/P0.4 + GPU (macOS) and P0.1/P0.2/P0.3/P0.4 (Linux). |

**Overall: P0 is not achieved as a single demonstration.** P0.1–P0.4 each pass where their hardware
exists, but the single-host flagship demo with OCI and a GPU together is blocked by hardware (#26).
Nothing here authorizes a migration.

## Integrated runs

Same graph on both hosts (`rust/examples/p0/`): synthetic 1280×720 camera into the shared-memory
arena → GPU chain (invert → normalize → mean) → stats; camera → live-replaceable detector → PLC
simulator; camera → exactly-once recorder. Update plan: v2 detector promoted at cycle 50; a
never-ready candidate (rejected, v2 keeps serving); a crashing candidate (promoted, rolled back).

### macOS, Apple M4 (Metal), detector candidates as Python workers

```bash
cd rust && cargo run --release -p mvf-cli -- run examples/p0/pipeline-gpu.yaml --updates examples/p0/updates-local.yaml --verify --expect-promotions 2 --expect-rollbacks 1 --max-cutover-us 1000 --require-gpu
```

```text
900 cycles, source-exhausted; det backend=worker gen=2; GPU nodes backend=compute
promoted    cycle 56   gen 1 -> 2  prep=50.2 ms  cutover=4 us
rejected    cycle 457  gen 2 -> 3  [replace.readiness-failed]
promoted    cycle 503  gen 2 -> 4  prep=31.4 ms  cutover=3 us
rolled-back cycle 503  gen 4 -> 2  [replace.candidate-failed]  cutover=26 us
data plane: copiesIn=0 copiesOut=0 transformWrites=900 zeroCopyHandoffs=846 inUse=0
gpu Apple M4: uploads=900 upload_bytes=829440000 downloads=900 download_bytes=230400
              device_handoffs=1800 dispatches=2700 allocated=0 peak_allocated=4608256
cycle ms p50=12.994 p95=13.386 p99=13.862 max=15.866  fps=78.0
VERIFY PASS (exactly once 900/900, 900 PLC commits / 900 cycles, cut-over ≤ 26 us)
/usr/bin/time: 11.96 s real, 6.26 s user, 0.44 s sys, peak RSS 29.4 MB
```

### Linux test host (Docker Desktop VM, linuxkit 6.10 aarch64), detector candidates as OCI images

```bash
rust/scripts/p0-testhost.sh
```

No Python and no Docker daemon inside; `runc` 1.1.5; GPU nodes fall back to their CPU builtins
(placement notes) because the VM has no GPU.

```text
2400 cycles, source-exhausted; det backend=oci gen=2
promoted    cycle 87   gen 1 -> 2  prep=533.3 ms  cutover=67 us     (OCI image: unpack + container start + Python start)
rejected    cycle 812  gen 2 -> 3  [replace.readiness-failed]       (OCI candidate never signals ready)
promoted    cycle 1532 gen 2 -> 4  prep=597.3 ms  cutover=88 us
rolled-back cycle 1532 gen 4 -> 2  [replace.candidate-failed]  cutover=180 us   (OCI candidate exits 9)
data plane: copiesIn=0 copiesOut=0 transformWrites=4800 zeroCopyHandoffs=2315 inUse=0
cycle ms p50=20.018 p95=21.750 p99=22.654 max=29.095  fps=50.1
VERIFY PASS (exactly once 2400/2400, 2400 PLC commits / 2400 cycles, cut-over ≤ 180 us)
/usr/bin/time: 47.92 s wall, 19.12 s user, 1.71 s sys, peak RSS 23.0 MB
```

A second, confirming run (same script) additionally checks `runc list` afterwards: **0 containers
left**; promotions at cycles 84 and 1530, rejection at 807, rollback at 1530 with cut-over 201 µs;
p50 20.8 ms, fps 48.5; VERIFY PASS.

The first Linux attempt failed honestly and exposed a deployment bug: as root the default OCI state
directory lived under `/root` (0700), which the remapped container root cannot traverse. Fixed:
default `/var/lib/mvf/oci` for root, plus a clear error when a state path is not traversable.

## Thresholds

| # | Threshold | Result |
|---|---|---|
| T1 | every cycle exactly once, in order | PASS (900/900 macOS, 2400/2400 Linux) |
| T2 | no duplicate PLC commit | PASS |
| T3 | no mandatory CLR | PASS (`mvf-rs` has no .NET dependency) |
| T4 | real OCI image, `runc`, no daemon / host Python | PASS (Linux test host) |
| T5 | sandbox properties | PASS (stage 08 e2e: `CapEff=0`, `NoNewPrivs=1`, `Seccomp=2`, empty route table, cgroup limits) |
| T6 | `copiesIn = copiesOut = 0` on the shared-memory path | PASS (both runs) |
| T7 | `uploads = frames`, ≤ 256 B/frame back from the GPU chain | PASS (900 uploads, 230 400 B = 256 B × 900) |
| T8 | GPU vs CPU mean error ≤ 1e-5 | PASS (stage 09 tests) |
| T9 | promotions/rollbacks as scripted; failed readiness rejected | PASS (both runs: 2 / 1, readiness rejection) |
| T10 | cut-over ≤ 1000 µs | PASS (max 26 µs macOS, 180 µs / 201 µs in the two Linux runs) |
| T11 | no leaked arena slot, device memory, container or worker | PASS (`inUse=0`, `allocated=0`; 0 containers left in the confirming Linux run; workers reaped per stage 03 tests) |
| T12 | Rust ≥ .NET throughput and ≤ .NET peak RSS on every baseline row | PASS (gate-a-report: all 6 rows; publish paths differ by design, see there) |
| T13 | GPU ≥ CPU throughput at 1920×1080 | PASS (782.6 vs 411.0 fps) |

## Compatibility and security matrix

| Backend | macOS arm64 | Linux | Windows | Isolation | Zero copy | Replacement |
|---|---|---|---|---|---|---|
| in-process | tested | tested (CI) | tested (CI) | none (engine domain) | host (`Arc`/lease) | stateless, stateful |
| worker (stdio v1) | tested | tested (CI) | tested (CI) | process (not a security boundary) | host (arena) | stateless, stateful (#24) |
| OCI (`runc`) | n/a (no Linux kernel) | tested (test host, root) | **BLOCKED** (WSL2/HCS) | sandbox (namespaces, no caps, no-new-privs, seccomp denylist, cgroups, device deny) | host (arena bind mount) | stateless |
| compute (wgpu) | tested on Apple M4 | builds in CI, **no GPU to test** | builds in CI, **no GPU to test** | none (trusted kernels) | device (same context) | stateless |

Known security limits: shared arena readable/writable by every worker and container that maps it;
seccomp is a denylist; no AppArmor/SELinux; rootless OCI not exercised; GPU kernels are trusted
in-process code. Details: stages 04 and 08.

## Not done (explicitly)

- Shadow evaluation of a candidate with side effects suppressed (optional in #20) — not
  implemented.
- GPU overlap admission (budget for a candidate's device memory during preparation) — not
  implemented; the software budget applies per context.
- Cross-process GPU buffers, CDI device injection, Windows OCI.
- Added after the first evaluation: checkpoint/restore, backpressure policies and the restart policy
  (#24, [stage 11](stages/11.md)); graph primitives, bindings, live operation and the pipelined
  executor (#25, [stages 12](stages/12.md)–[13](stages/13.md)).

Follow-up issues are opened for these; the existing C# host is not retired.
