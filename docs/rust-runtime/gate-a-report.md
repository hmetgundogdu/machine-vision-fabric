# Gate A report — foundation experiment (#11–#16)

**Gate A: PASS.** Machine-readable: [gate-a-report.json](gate-a-report.json). Decision record:
[ADR-0001](adr-0001-gate-a-foundation.md).

## Reproduce

From the repository root (Python 3 on `PATH`; CMake + a C++ compiler for the C++ interop test):

```bash
rust/scripts/gate-a.sh
```

Windows:

```bash
pwsh rust/scripts/gate-a.ps1
```

The foundation demo alone — source → live-replaced processor → Python classifier → sink, with a
promotion to a Python worker and a rollback, self-verified (exit code 3 if any check fails):

```bash
cd rust && cargo run --release -p mvf-cli -- run examples/live-update/pipeline.yaml --updates examples/live-update/updates.yaml --verify --expect-promotions 2 --expect-rollbacks 1
```

## Environment

| | |
|---|---|
| Local | macOS 26.6.2 (25G83), Apple M4 (10 cores), arm64 |
| Toolchain | rustc 1.95.0 (59807616e 2026-04-14), pinned by `rust/rust-toolchain.toml`; `Cargo.lock` committed |
| Workers | Python 3.9.6 (numpy 1.26.4), AppleClang + CMake for the C++ SDK example |
| .NET reference | .NET SDK 10.0.101, `Mvf.slnx` Release, unchanged |
| CI | GitHub Actions `Rust runtime` job on ubuntu-latest, windows-latest, macos-latest with Python 3.12 and the C++ SDK example built (`MVF_REQUIRE_CPP=1`) |

## Per-stage results

| Stage | Issue | Commit | Status | Report |
|---|---|---|---|---|
| 01 Workspace + Universal Node Contract | #11 | `8bd2c71` | PASS | [01.md](stages/01.md) |
| 02 Typed graph + deterministic executor | #12 | `692f464` | PASS | [02.md](stages/02.md) |
| 03 Backend API + lifecycle supervisor | #13 | `37f91e4` | PASS | [03.md](stages/03.md) |
| 04 CPU shared-memory data plane | #14 | `b68f43b` | PASS | [04.md](stages/04.md) |
| 05 Live replacement + rollback | #15 | `7a5e07c` | PASS | [05.md](stages/05.md) |
| 06 Gate A demo, regression, ADR | #16 | this commit | PASS | [06.md](stages/06.md) |

CI for stages 01–04 is green on all three operating systems (runs on the pushed branch; stage 05/06
results are recorded in the issue comments when the runs finish).

## Regression suite (local, at stage 06)

`cargo test --workspace --locked` → **110 passed, 0 failed**:

| Crate / suite | Tests |
|---|---|
| mvf-contract: fixtures / negotiation / yaml_limits | 6 / 9 / 9 |
| mvf-graph: validation | 12 |
| mvf-engine: unit / executor / replacement | 3 / 13 / 9 |
| mvf-memory: arena / descriptor | 10 / 5 |
| mvf-worker: worker / dataplane / replacement (real Python + C++ processes) | 16 / 8 / 3 |
| mvf-cli: end-to-end (incl. the Gate A demo) | 7 |

`cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`: clean.
The existing .NET, Python SDK, TypeScript client and C++ SDK CI jobs are unchanged and green.

## Demo output (local, release build)

```text
replace proc  requested    at cycle 20   gen 1 -> 2
replace proc  promoted     at cycle 29   gen 1 -> 2   prep=55.8ms   cutover=12us     (Rust -> Python worker)
replace proc  committed    at cycle 39   gen 1 -> 2
replace proc  requested    at cycle 50   gen 2 -> 3
replace proc  promoted     at cycle 51   gen 2 -> 3   prep=0.1ms    cutover=12us
replace proc  rolled-back  at cycle 70   gen 3 -> 2   [replace.candidate-failed]   cutover=5us
data plane: copiesIn=0 copiesOut=0 transformWrites=221 zeroCopyHandoffs=193 inUse=0
verify every-cycle-exactly-once-in-order  PASS  sink.frame: 120 records for 120 cycles
VERIFY PASS
```

## Baseline against the .NET engine

Procedure: `rust/scripts/bench-baseline.sh` (2000 cycles per row). Both engines run the same
generated frames (constant 128, 4 files, as `packages/bench/bench.sh`), the same unmodified Python
modules (`py.bench-touch`, `py.bench-numpy`), the same topology (looping folder source → Python
classifier, no sink) and serial executors. One run per row, 2026-11-26, the local machine above.

```text
eng   mode     frame MB |  engine s |   rate f/s |    cycles |     node ms | process s |   rss MB
.NET  touch     0.06 MB |    0.26 s |   7692 f/s |         - |     0.04 ms |    0.42 s |    72 MB
Rust  touch     0.06 MB |    0.09 s |  23027 f/s |     0.04s |    0.019 ms |    0.09 s |    11 MB
.NET  numpy     0.06 MB |    0.55 s |   3636 f/s |         - |     0.17 ms |    0.68 s |    74 MB
Rust  numpy     0.06 MB |    0.31 s |   6370 f/s |     0.28s |    0.135 ms |    0.31 s |    29 MB
.NET  touch     1.00 MB |    0.83 s |   2410 f/s |         - |     0.04 ms |    0.96 s |    83 MB
Rust  touch     1.00 MB |    0.10 s |  19619 f/s |     0.07s |    0.017 ms |    0.10 s |    14 MB
.NET  numpy     1.00 MB |    1.56 s |   1282 f/s |         - |     0.46 ms |    1.70 s |    86 MB
Rust  numpy     1.00 MB |    1.01 s |   1981 f/s |     0.97s |    0.467 ms |    1.01 s |    30 MB
.NET  touch     6.00 MB |    4.60 s |    435 f/s |         - |     0.07 ms |    4.74 s |   159 MB
Rust  touch     6.00 MB |    0.28 s |   7267 f/s |     0.24s |    0.016 ms |    0.28 s |    44 MB
.NET  numpy     6.00 MB |    7.40 s |    270 f/s |         - |     2.08 ms |    7.53 s |   144 MB
Rust  numpy     6.00 MB |    4.49 s |    446 f/s |     4.45s |    2.093 ms |    4.49 s |    44 MB
```

How to read it — and how not to:

- **engine** is each engine's own run duration. .NET: `ExecuteAsync` (activation + cycles). Rust:
  `wallTimeMs` (start + cycles + shutdown). **process** includes runtime start-up (CLR + JIT for
  .NET). **rss** is `/usr/bin/time`'s peak RSS, which is the max over the host and the worker
  processes it reaped, not a sum.
- The **publish paths differ by design**, and that dominates the large-frame `touch` rows. The .NET
  path streams each frame from its envelope into a `MemoryStream`, calls `ToArray()` and copies the
  result into the arena (`DataPlaneRouter.TryPublishFrameForWorkersAsync`); the Rust
  `folder-source` caches the files at activation and does exactly one counted copy into the arena
  per frame (verified: `bytesCopiedIn` = 6.0 MiB × cycles). Do **not** read the 6 MB `touch` row as
  "Rust is 16× faster than C#"; it says the Rust publish path does less work.
- Where Python work dominates (`numpy`), the Python node costs the same in both engines
  (2.08 ms vs 2.09 ms at 6 MB); the remaining gap is the per-cycle publish cost above.
- Start-up: the Rust CLI process finishes a 2000-cycle 64 KB run in 0.09 s vs 0.42 s for the .NET
  CLI, most of which is CLR start-up and JIT.
- Not measured: p95/p99 latency, jitter, CPU utilisation, binary size, the pipelined .NET executor
  (the Rust runtime has no pipelined executor), long-running stability. Single runs, no variance
  analysis. These belong to the P0 benchmark plan (#20).

## Known limitations carried into Gate B

See [ADR-0001 § Unresolved](adr-0001-gate-a-foundation.md#unresolved-explicitly-not-solved-by-gate-a)
and [parity-matrix.md](parity-matrix.md). Nothing in Gate A implements OCI, GPU or security
sandboxing; contracts that require them are rejected by negotiation.
