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
