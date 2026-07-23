# Data-plane benchmark

Pushes realistically-sized frames through the .NET → Python shared-memory path and reports throughput.
No hardware: the source is the folder simulator, and the rig generates its own frames.

```bash
./packages/bench/bench.sh              # 2000 cycles per row (~30s)
CYCLES=200 ./packages/bench/bench.sh   # quick pass
SKIP_BUILD=1 ./packages/bench/bench.sh # skip the build step
```

The script locates the repo from its own path, so it runs from anywhere. It refuses to print numbers from
a run that did not succeed or that restarted a worker mid-flight — a benchmark that quietly measures a
degraded run is worse than no benchmark.

Two pipelines, same source:

| pipeline | worker does | measures |
|---|---|---|
| `pipeline-touch.json` | touches the frame, no compute | shared-memory handoff + stdio round-trip |
| `pipeline-numpy.json` | zero-copy numpy view over the **whole** frame, means it | realistic full-frame processing |

Neither has a sink, so no disk-write noise.

## Recorded baseline — serial executor

**This is the number the pipelined executor has to beat.** Recorded 2026-07-23 on Apple M4 (10 cores),
macOS 26.5.2, .NET 10.0.101, python 3.9.6 + numpy 2.0.2, `CYCLES=2000`.

```
mode        frame | throughput | throughput |        node |     py rtt |
touch     0.06 MB |  7692 f/s |   0.50 GB/s | node   0.04 ms | rpc   0.04 ms | 0.26s/2000 cyc
numpy     0.06 MB |  6250 f/s |   0.41 GB/s | node   0.09 ms | rpc   0.08 ms | 0.32s/2000 cyc
touch     1.00 MB |  2174 f/s |   2.28 GB/s | node   0.05 ms | rpc   0.04 ms | 0.92s/2000 cyc
numpy     1.00 MB |  1389 f/s |   1.46 GB/s | node   0.44 ms | rpc   0.43 ms | 1.44s/2000 cyc
touch     2.00 MB |  1149 f/s |   2.41 GB/s | node   0.05 ms | rpc   0.05 ms | 1.74s/2000 cyc
numpy     2.00 MB |   712 f/s |   1.49 GB/s | node   0.80 ms | rpc   0.79 ms | 2.81s/2000 cyc
touch     6.00 MB |   426 f/s |   2.68 GB/s | node   0.08 ms | rpc   0.07 ms | 4.70s/2000 cyc
numpy     6.00 MB |   212 f/s |   1.34 GB/s | node   2.33 ms | rpc   2.32 ms | 9.42s/2000 cyc
```

`node` is the engine-side cost of the whole worker node; `rpc` is the worker round-trip nested inside it.
The gap between them (~0.01 ms) is engine overhead — marshalling and the handle handoff.

## What the numbers say

Per-node time does **not** add up to wall clock, and the difference is the point. From a single 6 MB numpy
run (2000 cycles, 8.95 s = **4.48 ms/cycle**):

```
  cam:   avg=0.01ms          # just pulls an already-buffered frame off the source channel
  bench: avg=2.26ms          # of which rpc=2.25ms — python's numpy mean over 6 MB
```

That accounts for 2.27 ms of a 4.48 ms cycle. The missing ~2.2 ms is the **arena publish** — the memcpy of
the frame into the shared-memory slot, which happens during edge routing and is therefore attributed to no
node. At ~2.7 GB/s (see the `touch` rows) a 6 MB copy is ~2.3 ms, which matches.

So a serial cycle is roughly: pull (0.01) → publish 6 MB (2.2) → python computes (2.26). Three costs paid
back to back because only one frame is ever in flight.

**Prediction for the pipelined executor:** overlapping the publish with the worker's compute should take
the 6 MB numpy row from ~212 f/s toward ~425 f/s — near 2×, bounded by `max(stage)` instead of `Σ(stage)`.
Re-run this rig after phase 1 and put the new table next to this one. If the win is much less than that,
the bottleneck is somewhere this rig does not yet show.

## Known gap

Arena publish time is unattributed — it belongs to an edge, not a node, and there is no per-edge timing
yet. That is fine while execution is serial (wall clock minus node time *is* the publish cost), but the
pipelined executor makes routing a real stage, and it should be measured as one then.
