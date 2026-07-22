# Data Plane — Design (agreed with user)

> Status: **design agreed at the decision level**; implementation still to be scheduled.
> Companion to `roadmap.md` (M2). Hard constraint: **local only, no network.** Data moves
> over **engine-owned shared memory** (zero-copy handles); control moves over **stdio**.

## Model in one paragraph
The engine is the central orchestrator: it schedules every node each cycle and hands out
handles. So the shared memory is "just" an **engine-owned arena** of buffers plus a
metadata region (free-list, refcount table, leases). Big data (frames/tensors) lives in the
arena and is passed **by handle**; small control/results travel over stdio. Because the
engine drives the cycles, readiness / backpressure / crash-cleanup collapse into the
scheduler instead of needing separate IPC machinery.

## Agreed decisions
1. **Ownership & mutation.** A node may freely **mutate the buffer it produces** while
   executing (it owns its own output). The emitted result is a **finalized value that is
   read-only to consumers** and **may be null** (a node can emit nothing this cycle — e.g. a
   filter/classifier that drops a frame). No node mutates another node's buffer.
2. **Slot sizing: variable, auto-detected.** Buffers are sized to what the module actually
   needs — the module requests a size (or the engine auto-detects from the produced payload).
   The engine arena is a **variable-size allocator**, not fixed slots.
3. **Refcount: precomputed from the static graph.** Initial refcount of an emitted buffer =
   number of downstream consumers of that edge (known at build time). Each consumer
   decrements on "done"; at 0 the buffer is reclaimed. (Our graph-aware advantage.)
4. **Pool ownership: the engine.** The engine creates and owns the shared segment, the
   free-list, the refcount table and the lease table (single source of truth). Modules only
   **map** the segment and read/write their buffers via handles.
5. **Descriptor in the slot header.** Every allocation is `[header | payload]`. The header
   carries the typed descriptor (dataType, w/h/stride/format, seq, timestamp, refcount,
   owner/lease). The payload is raw bytes. Modules read the header directly.

### Added by user
6. **Engine-controlled context slot.** A shared, engine-owned **context region** for general
   information sharing across modules (a typed blackboard: run config, shared parameters,
   cross-node signals). Engine governs writes; modules read (and write where allowed).
7. **Engine-allocated module state.** The engine allocates each module's **durable state**
   inside shared memory too — not just data buffers. A module keeps its checkpointable state
   in an engine-provided **state slot** rather than in its own private heap.

## Snapshot & crash recovery (user's vision: "100%")
Because both **data** and **module state** live in engine-owned shared memory, the engine can
**snapshot the whole shared region** (arena + context + per-module state) periodically and in
parallel, and on a crash **restore the snapshot and resume where it left off.**

**Consistency:** snapshots are taken at **cycle boundaries** — the engine already quiesces
between cycles, so those are natural, torn-free checkpoints (no need for a global lock during
a cycle). A copy-on-write snapshot is a later optimization if we want snapshots without pausing.

**Honest caveat — what "100%" really means.** Snapshotting recovers everything that is plain
memory. It does **not** by itself recover **external resources**: an open camera socket, a
model loaded into GPU memory, a file handle, a PLC connection. Those are not bytes in our
arena — they live in the OS/driver/kernel. So the contract is:
- Snapshot restores all **in-memory state** (arena + context + state slots) exactly.
- On restore, each module **re-establishes its external resources from its state slot**
  (reconnect the camera, reload the model, reopen files) — a small **rehydration** step.
This is the standard actor/event-sourcing checkpoint pattern. With it, the system is
deterministically recoverable; without the rehydration step, "100%" is not achievable for any
runtime, because kernel-owned handles can't be memcpy'd. The design supports the full vision;
the rehydration contract is the one honest piece modules must implement.

## Memory layout (engine-owned segment)
```
[ metadata region ]   free-list, refcount table, lease table (per-process), snapshot header
[ context slot    ]   engine-governed shared blackboard
[ module state    ]   one state slot per module instance (checkpointable)
[ data arena      ]   variable-size [header|payload] allocations, refcounted
```

## Why the engine-orchestrated model keeps it simple
- **Readiness signal** = the control message ("run node X with handle H"). No separate futex.
- **Backpressure** = if the arena can't satisfy an allocation, the scheduler doesn't run the
  producer until space frees (or applies a per-source drop policy).
- **Crash cleanup** = engine supervises child processes; on death it reclaims that process's
  leases and can restore from the last snapshot.

## Open design points (decide before implementation)
- **Allocator strategy** for variable sizes: size-class/slab pools vs bump+compaction vs
  buddy allocator (fragmentation vs speed).
- **Snapshot mechanism**: cycle-boundary quiesce (simple) vs copy-on-write (non-blocking).
- **SDK surface** for the state slot + the rehydration hook modules implement.
- **Descriptor/typing enforcement**: header is in shared memory, but the engine still
  validates types at the boundary (don't trust a module's header blindly).
- **GPU buffers** (later): same handle model but the payload lives in GPU memory (CUDA-IPC/
  DLPack); still local, still no network.

## Milestone mapping
- **M2** = arena + handles + refcount + descriptor header + context slot (the core data plane).
- **Snapshot + module-state recovery** = its own milestone (M2.5 / folded into M3 hardening),
  after the core data plane works. It is the ambitious, differentiating capability — sequence
  it after the basics are solid.

---

## Implementation decisions (agreed with user, 2026-07-22)
These turn the design above into buildable choices. Confirmed together:
1. **Arena backing = file-backed MMF.** One real file that both .NET (`MemoryMappedFile.CreateFromFile`)
   and Python (`mmap`) map. Chosen for interop (named OS shm doesn't interoperate cleanly between
   .NET and Python, especially on Windows) and because snapshot = copy the file. On Linux back it
   with tmpfs; on Windows the file stays in the page cache (RAM). All internal references are
   **offsets from the arena base, never raw pointers** (each process maps at a different address).
2. **Allocator = segregated free-list (slab / size-classes).** O(1) alloc/free, snapshot-friendly
   (offsets never move — no compaction). Start with a **single size-class** and generalize later.
   Buddy/bump+compaction rejected (compaction would move handles).
3. **Execution is serial within a cycle** (engine runs nodes one at a time, topologically), so a
   produced buffer has one writer and its readers run later in the same cycle — **no payload locks**.
   Only the free-list/refcount table needs guarding, and only if we ever parallelize.
4. **Transport is a separate project** `src/transports/Mvf.Transport.SharedMemory`; the **core
   (Graph/Abstractions/Engine) never references it**. Hosting/composition layers wire it in.

### Slice A — smallest valuable cut (in progress)
Replace the **base64-over-stdio frame payload** on the Python worker path with a **shared-memory
handle**. Today `WorkerFrameClassifier` base64-encodes the frame and ships it down the pipe (several
copies + encoding). Slice A: the .NET side copies the frame into the arena **once**, sends a small
`{offset,length}` handle over stdio; Python `mmap`s the arena file and reads in place — no base64.

Deliberate Slice-A simplifications (each lifted in a later slice):
- **Arena lives in the worker-hosting layer for now**, not the engine — Slice A is the *only* path
  that needs shared memory (a .NET source produces a heap frame; only the polyglot hop copies it in).
  When .NET producers/consumers also use the arena (Slice B), the arena is lifted behind an
  `IDataPlane` seam in `Mvf.Abstractions` — its shape then known from real use, not guessed.
- **Refcount is trivially 1**: the frame has exactly one consumer (the classifier), rented and
  returned around a single RPC. Graph-derived refcounts arrive with fan-out in Slice B.
- **Descriptor rides the stdio execute message** (cameraId/sequence/contentType are small); the slot
  holds only payload bytes. Full descriptor-in-slot-header lands when a .NET↔.NET hop (no stdio)
  needs it.
- **Free-list/refcount stay in .NET managed memory** (only .NET allocates in Slice A; Python only
  reads). They move into the shared metadata region when a second process allocates, or for snapshot.
- **base64 stays as a fallback** when no arena is present or a frame exceeds the slot size, so the
  direct-construction path keeps working.

Slice A touchpoints: new `Mvf.Transport.SharedMemory` (arena + `FrameHandle`); `Mvf.Hosting.Worker`
uses it (arena path passed to the child via env at spawn; handle in the execute message); Python SDK
maps the arena and reads at the handle; `protocol/README.md` documents the shm frame form.

### Slice B — engine-owned arena + graph-aware routing (decisions agreed 2026-07-22)
Chosen appetite: **the depth machinery** (the graph-aware differentiator), not just more polyglot
breadth. Agreed decisions:
- **`IDataPlane` seam in `Mvf.Abstractions`.** The engine owns one data plane per run and references
  only the interface; the concrete file-backed arena stays in the transport project (core never
  references it). Composition wires one instance, shared by the executor and the worker host.
- **Payload-generic, not frame-locked.** The arena stores **opaque bytes**; the **type comes from the
  static typed graph** (`data/frame` today, `data/tensor` etc. later) — nothing in the arena is
  frame-specific. `FrameHandle` → **`ArenaHandle(offset,length)`**; the frame-specific
  `ArenaFrameEnvelope` is just one typed adapter over a handle. The `dataType` also rides the stdio
  handle message so a worker knows what it is reading.
- **Transport selection is per-edge, from the static graph.** Both endpoints in-process .NET →
  **identity** (heap reference, zero copy). A port with **≥1 worker consumer** → publish the payload
  into the arena **once** and route the same arena-backed value to *all* consumers of that port.
- **Refcount = number of consumers of that port** (precomputed from the graph), set at publish. Each
  consumer's completion decrements; **reclaim at 0** (cycle boundary at the latest). The free-list +
  refcount table stay **engine-side (.NET managed)** — Python still never allocates; it only reads an
  input handle (and, later, writes a pre-assigned output handle).

**Staging (build infra first, per user):**
- **B.1 — infra, behavior-preserving:** `IDataPlane` + `ArenaHandle` in Abstractions; arena implements
  it and gains a refcount (publish-with-count / release-to-zero) + lazy backing file; the data plane is
  injected (engine-owned) rather than self-created by the host. Slice A's path keeps working through
  the seam.
- **B.2 — graph-aware routing:** precompute per-port consumer counts + worker-vs-inproc; publish-once +
  route an `ArenaFrameEnvelope` to all consumers; executor decrements after each consumer. Demo: one
  .NET source frame fans out to **two** Python nodes sharing **one** arena copy (refcount 2), reclaimed
  at cycle end.

**Deferred (after the infra):** the **transformer** capability (a worker that emits a *new* frame — a
new capability .NET itself lacks; engine pre-allocates the output slot and passes both handles in
`execute`, so no child-side allocator) and the **context slot**. **Slice C = M2.5**: module state slot
+ snapshot + resume.
