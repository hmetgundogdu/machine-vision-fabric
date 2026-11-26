# P0 go/no-go thresholds (fixed before the final #20 runs)

Committed **before** the integrated P0 runs of #20. They were chosen after the component
measurements of stages 06–09 (so they are informed by those exploratory runs, not blind), and they
are not changed after the final runs. A P0 gate is PASS only if every threshold of that gate is met
on real hardware; a gate whose prerequisite hardware/OS is missing is BLOCKED, never PASS.

| # | Gate | Metric | Threshold |
|---|---|---|---|
| T1 | P0.1 graph | Frames reaching the PLC simulator / recorder | every completed cycle exactly once, in order (`--verify`) |
| T2 | P0.1 graph | Committed PLC effects | no duplicate cycle id (the simulator refuses a second commit) |
| T3 | P0.1 graph | Mandatory CLR | none: `mvf-rs` has no .NET dependency |
| T4 | P0.2 OCI | OCI node execution | real image, `runc`/`crun`, no Docker daemon or host Python in the test host |
| T5 | P0.2 OCI | Sandbox | `CapEff=0`, `NoNewPrivs=1`, `Seccomp=2`, empty routing table, limits visible in cgroupfs |
| T6 | P0.3 CPU | Transfer copies on the shared-memory path | `copiesIn = 0` and `copiesOut = 0` for arena-resident sources |
| T7 | P0.3 GPU | Host round trips between GPU nodes | `uploads = frames`, `downloads ≤ frames`, ≤ 256 bytes downloaded per frame by the GPU chain |
| T8 | P0.3 GPU | Correctness vs CPU | per-frame mean error ≤ 1e-5 |
| T9 | P0.4 replacement | Promotions / rollbacks in the scripted plan | exactly as scripted; a failed-readiness candidate is rejected and v1 keeps serving |
| T10 | P0.4 replacement | Cut-over pause (`cutoverUs`) | ≤ 1000 µs for every promotion and rollback |
| T11 | all | Leaks | arena `inUse = 0`, GPU `allocated = 0`, no container or worker left after the run |
| T12 | baseline | Rust vs .NET serial engine on `bench-baseline.sh` rows | Rust throughput ≥ .NET on every row; Rust peak RSS ≤ .NET on every row |
| T13 | baseline | GPU vs CPU chain at 1920×1080 | GPU throughput ≥ CPU throughput |

Not thresholds (reported only): p50/p95/p99 cycle latency, fps, preparation time of candidates,
CPU time, peak RSS of the integrated runs, peak device memory.
