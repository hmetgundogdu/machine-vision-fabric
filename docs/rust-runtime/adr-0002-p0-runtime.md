# ADR-0002 — P0 runtime: OCI, GPU and the decision after Gate B

- **Status:** Accepted for the research branch. Not a migration decision.
- **Date:** 2026-11-26
- **Issues:** #10, #17–#20 · builds on [ADR-0001](adr-0001-gate-a-foundation.md)
- **Evidence:** [p0-report.md](p0-report.md), [gate-b-report.json](gate-b-report.json),
  [competitive-landscape.md](competitive-landscape.md), stages 07–10

## Decisions

1. **OCI images are content, not a daemon.** `mvf-oci` imports OCI layouts and Docker archives
   offline into a digest-verified store and exports them back byte-identically; Docker is optional
   preparation/test tooling. Rejected: a registry client in the runtime (not needed for edge
   offline operation; `docker`/`skopeo` can prepare archives).
2. **Execute OCI through a proven low-level runtime.** `runc`/`crun` driven with a generated bundle,
   not a re-implementation of namespaces/cgroups. Youki fits the same seam. An OCI node is a worker
   whose launcher is the runtime (`Launcher` trait), so supervision, the stdio protocol, live
   replacement and arena zero copy are shared code.
3. **Sandbox by default, grants explicit.** Contract `spec.permissions` (`network`, `devices`,
   `cdiDevices`) is negotiated fail-closed; isolation `sandbox` is only advertised with seccomp.
4. **Vendor-neutral GPU first.** `wgpu`/WGSL (Metal, Vulkan, DX12) for the compute backend; device
   payloads in the engine whose host bytes need an explicit, counted transfer. CUDA/TensorRT and
   cross-process GPU sharing are later backends behind the same seams.
5. **Measure before claiming.** Thresholds committed before final runs; every verdict in the P0
   report is tied to a run or test.

## Consequences

- The universal contract now spans four backends with one negotiation and one replacement protocol
  (in-process → Python worker → OCI image, with rollback) — the combination the thesis is about.
- Two real limits shape the next step: GPU buffers do not cross process boundaries, and no tested
  host has OCI and a GPU together.
- The rootfs cache is per image manifest (each packaged image unpacks its full base); layer-level
  caching would cut candidate preparation time.

## Decision on proceeding

P0 is **not** achieved as a single demonstration (the single-host demo is blocked by hardware).
P0.1 was first evaluated as incomplete; it passes since #24 (recovery) and #25 (primitives,
pipelined executor). Recommendation:

1. keep the .NET engine as the shipping implementation and the reference;
2. continue the Rust runtime as research on this branch, prioritising a Linux + GPU host for the
   single-host P0 run (#26), Windows OCI (#27) and worker/arena hardening (#28);
3. revisit migration only after those pass; do not merge to `main` or release from this branch.
