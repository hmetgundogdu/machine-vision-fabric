# Closest alternatives — what exists, what overlaps, where MVF is (and is not) different

Versions pinned from each project's GitHub releases/tags on **2026-11-26**. Capability statements
come from the project's own README or release notes at that version (primary sources, linked).
"Not described" means the primary source does not claim it; it does **not** mean the project
cannot do it. MVF claims are limited to what stages 01–10 measured.

| Project | Pinned version | Primary source |
|---|---|---|
| dora-rs | v1.1.0 (2026-10-08) | [README](https://github.com/dora-rs/dora), [releases](https://github.com/dora-rs/dora/releases) |
| NVIDIA Holoscan SDK | v5.0.0 (2026-10-21) | [README](https://github.com/nvidia-holoscan/holoscan-sdk), [4.2 release blog](https://nvidia-holoscan.github.io/blog/2026-05-13-holoscan-sdk-4.2/) |
| Eclipse iceoryx2 | v0.10.1 (tag) | [README](https://github.com/eclipse-iceoryx/iceoryx2) |
| youki | v0.8.0 (2026-11-23) | [README](https://github.com/youki-dev/youki) |
| runwasi | containerd-shim-wasmtime/v0.6.1 (2026-06-12) | [README](https://github.com/containerd/runwasi) |
| Wasmtime | v51.0.0 (2026-11-20) | [README](https://github.com/bytecodealliance/wasmtime) |
| Apache TVM | v0.27.0.post1 (2026-09-29) | [README](https://github.com/apache/tvm) |
| GStreamer | 1.28.9 (latest stable tag on the GitHub mirror) | [GStreamer](https://gstreamer.freedesktop.org/) |

## Capability matrix

| Capability | MVF Rust runtime (measured here) | Closest existing |
|---|---|---|
| Typed, declarative graph | YAML, strict loader, separate data/control edge lists, runtime re-validation | **dora-rs**: "Declarative YAML dataflows … typed inputs/outputs, optional type annotations with static validation". GStreamer: caps negotiation between elements. Holoscan: operator graphs (C++/Python). |
| Multi-language nodes | Python and C++ SDK modules over one stdio protocol; Rust in-process | **dora-rs**: "Rust, Python, C, or C++ with native APIs". iceoryx2: bindings for C, C++, Python, C#. |
| Zero-copy CPU hand-off between processes | Engine-owned arena, generation-checked leases; `copiesIn = copiesOut = 0` measured | **iceoryx2**: "Zero-Copy Lock-Free IPC". **dora-rs**: "zero-copy shared memory IPC for messages >4KB" via Zenoh SHM. |
| Device-resident GPU hand-off between nodes | Same-device `wgpu` buffers by handle; 1 upload + 256 B readback per frame measured (Apple M4) | **Holoscan**: GPU-centric sensor processing; its 4.2 release blog is titled "GPU-Resident DAGs". Holoscan targets NVIDIA hardware; MVF's path is vendor-neutral (wgpu) but only in-process. |
| Run OCI images without a daemon | `runc` driven directly, default-deny sandbox, typed node over stdio; measured on Linux | **youki** / runc: OCI runtimes (MVF uses this class of tool, does not replace it). runwasi: containerd shims for Wasm workloads (containerd-managed). Holoscan: distributed as NGC containers. |
| Sandboxed lightweight nodes (WASM) | **not implemented** (contract schema only) | **Wasmtime**: "Secure", WASI and the component model. runwasi integrates Wasm hosts with containerd. |
| Live node replacement with rollback | Generation-fenced cut-over at cycle boundaries, probation, replayed failed cycle; 4–180 µs cut-over measured | **dora-rs**: "Hot reload — live-reload Python operators without restarting", "Dynamic topology — add and remove nodes from running dataflows". Rollback/probation semantics are not described there. |
| Distributed / multi-machine | **not a goal** (local only, by MVF rule) | **dora-rs**: "Distributed by default … Zenoh pub-sub for cross-machine communication". |
| ML compilation / kernel generation | **not a goal** (kernels are hand-written WGSL) | **Apache TVM**: "open machine learning compilation framework". |
| Media pipelines, codecs, hardware decoders | **not implemented** | **GStreamer**: the established media framework (plugins, hardware acceleration). |
| Windows | in-process, worker, compute tested in CI; OCI **BLOCKED** (needs WSL2/HCS) | iceoryx2: Windows listed as "done" (tier 2). dora-rs: Windows install documented. |

## What this means for the MVF thesis

- **Not unique on any single axis.** Typed YAML dataflows, multi-language nodes, shared-memory zero
  copy and live node changes all exist in dora-rs; zero-copy IPC is iceoryx2's whole purpose; GPU
  graphs are Holoscan's; daemonless OCI execution is what runc/youki are for. MVF reuses runc
  rather than competing with it.
- **The measured differentiator is the combination under one contract**: one negotiated node
  contract (isolation level, permissions, memory domain, zero-copy requirement, replacement mode)
  across in-process, process, OCI-sandboxed and GPU backends, with fail-closed negotiation and a
  live-replacement protocol that works across those backends (in-process → Python worker →
  OCI image, with rollback). The P0 runs show this working on real hardware, but **not on one
  host with OCI and GPU together** (BLOCKED, see the P0 report). Whether that combination is worth
  more than adopting dora-rs or Holoscan plus a sandboxing layer is an open product question this
  research does not settle.
- **Gaps relative to the alternatives**: no distributed mode (by design), no WASM backend, no media
  framework integration, no NVIDIA-specific GPU path (CUDA/GPUDirect), single-host GPU sharing only
  in-process, a young and unfrozen contract schema.
