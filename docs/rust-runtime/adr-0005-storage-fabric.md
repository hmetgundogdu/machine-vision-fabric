# ADR-0005 — Storage Fabric: volumes, bindings and artifact lifecycle

- **Status:** Accepted for the research branch (#31). Not a migration decision.
- **Date:** 2026-11-27
- **Issues:** #31, part of #10. Related: #23 (Network Fabric), #27 (rootless and Windows OCI), #28
  (arena isolation), #30 (Display Fabric).
- **Evidence:** [stage 20](stages/20.md)

## Context

Inspection nodes need files: model weights, calibration, incoming images, results, logs. Before
#31:
- An OCI node saw a read-only rootfs, bounded tmpfs and the arena, and nothing else.
- A worker saw whatever its user could see, while its contract promised nothing.
- Modules found their files through ad-hoc config paths.

## Decisions

1. **The node declares needs; the deployment decides sources.**
   - **Contract:** `spec.storage.mounts` lists logical mounts. Each has a name, a sandbox target,
     an access (`read-only` by default), a kind (`volume`, or `scratch` with a `maxSize`) and
     `required`. `spec.storage.isolation` is `best-effort` or `required`.
   - **Pipeline:** `spec.volumes` declares what exists:

     | Type | What it is |
     |---|---|
     | `persistent` | MVF-managed, one node, retained |
     | `shared` | MVF-managed, several nodes, explicit `writers` |
     | `bind` | an operator-approved host directory, read-only unless `access: read-write` |
     | `artifact` | a package file pinned by `sha256` |

     Each node binds `storage: { mount: volume }`.
2. **No node gets more access than the volume allows.** Graph validation refuses:
   - read-write on artifacts and read-only binds;
   - read-write on a shared volume by a non-writer;
   - a persistent volume mounted by two nodes;
   - unbound required mounts, unknown volumes, unsafe host paths and malformed digests.

   Live-replacement rebinding goes through the same function.
3. **MVF provisions; the operating system does the I/O.** There is no file proxy and no file API.
   - The node opens `MVF_VOLUME_<NAME>`: a host path for a worker, the mount target for an OCI
     node. The same module code runs on both backends.
   - The Python SDK adds `volume()` and `write_atomic()` (temporary file, `fsync`, rename) for
     publishing records.
4. **Honest enforcement, fail closed.**

   | Backend | Mount set | Read-only | Scratch `maxSize` | `isolation: required` |
   |---|---|---|---|---|
   | OCI (Linux, root, runc) | **enforced**: mount namespace, only the binds are visible | **enforced**: kernel `ro` bind (`EROFS`) | **hard**: size-limited tmpfs (`ENOSPC`) | accepted |
   | worker | not enforced: the node runs with its user's filesystem; `placement.storage-not-isolated` | file permissions on artifacts only (root bypasses them) | advisory: the overrun is logged at release | **rejected** (`negotiation.storage-not-enforceable`) |
   | in-process | no mounts (`negotiation.storage-unsupported`) | — | — | rejected |

   - **Managed directories** are given to the sandbox's mapped uid.
   - **A read-write host bind** the sandbox uid cannot write is refused at start, not at the
     first failed write.
5. **Lifecycle.**
   - **Persistent and shared data:** never removed by a stop, a crash, a restart, a replacement or
     the engine exiting. Only `mvf-rs volume delete <pipeline>/<volume> --yes` removes it.
     Volumes are scoped to the pipeline's name.
   - **Scratch:** fresh per generation instance. A worker's scratch is a host directory removed
     when its generation goes, and swept after a crash (dead engine pid). A sandbox's scratch is a
     tmpfs that dies with the container.
   - **Artifacts:** imported once into a content store at `<root>/artifacts`, separate from the
     OCI image store and its GC. They are verified against the pinned digest, made read-only and
     deduplicated. Each generation gets its own read-only view (a hard link), so a replacement
     candidate and the generation it may roll back to are pinned to their own files at the same
     time. Re-hashing a cached blob on every start is opt-in (`MVF_STORAGE_VERIFY=always`).
6. **Live replacement.** A request may rebind mounts (`ReplacementRequest::with_storage`, or
   `storage:` in an update plan). The candidate is validated against the pipeline's volumes
   before it is prepared (`replace.storage-invalid`). Promotion moves the slot to the new
   bindings, and a rollback restores the previous generation's bindings and views.
   - **Mutable volumes are not versioned.** The engine never executes two generations of a node
     in the same cycle (no shadow execution), so a persistent volume has one writer at a time.
     But a rollback cannot undo what the candidate wrote. Compatibility of mutable state is the
     node's contract (`lifecycle.state` for state the engine migrates; data formats for files).
7. **Path safety.**
   - **Host paths** must be absolute and normalized, must not resolve through a symlink, and must
     not be `/`, a users root, the user's home or inside a system directory (`/etc`, `/proc`,
     `/var/lib/docker`, `C:\Windows`, ...). They are checked as written and again in canonical
     form.
   - **Artifact files** must be regular, package-relative files; symlinks are not followed.
   - **Mount targets** must stay outside the sandbox's own and system paths. A target that
     resolves through a symlink, or lands on a file the image ships, refuses the start.

## Platform matrix

| Combination | Status |
|---|---|
| Linux OCI, root, runc (no Docker daemon) | **PASS**: enforced mounts, read-only, tmpfs quotas, uid mapping |
| Linux OCI, rootless | **BLOCKED** (#27: rootless OCI not implemented) |
| Workers on macOS, Linux and Windows (CI) | **PASS** for provisioning, retention, pinning, publication and replacement. Isolation is not enforced and is labelled so |
| Windows: ACLs or AppContainer for worker isolation, HCS/WSL2 file mapping | **BLOCKED**: not implemented, no Windows OCI host |
| macOS: sandboxing workers (Seatbelt is deprecated), Linux OCI in a VM (virtio-fs) | **BLOCKED**: not implemented, no VM in scope |
| Hard quotas on persistent volumes (XFS/ext4 project quotas) | Not implemented: a volume's size is the filesystem's. Free space is checked for scratch |
| Network storage (SMB/NFS) | An operator mount bound as a host directory; no MVF network storage (#23) |

## Consequences

- **A developer writes ordinary file code.** The model inspector reads `*.onnx` from a directory
  and publishes JSON records. A sandbox enforces its storage; a worker runs the same code and the
  placement says it is not confined.
- **Storage stays off the graph's hot path.** Payloads still travel through shared memory: a
  transformer with a mounted artifact keeps zero copies and its per-frame cost (stage 20).
- **Not covered:** distributed storage, transactional rollback of arbitrary writes, quotas on
  persistent volumes, Windows/macOS sandboxing.

## Threat model

- **Protects against:**
  - an OCI node reading or writing anything but its mounts;
  - writing a read-only mount;
  - exceeding scratch;
  - a deployment exposing `/`, a home, system directories or a symlinked path;
  - an artifact that is not what was pinned;
  - an image that redirects a mount target;
  - leftovers of crashed engines.
- **Does not protect against:**
  - a trusted worker, which has its user's filesystem by declaration;
  - root on the host;
  - weaknesses of data formats the node parses;
  - a writer corrupting its own volume;
  - timing or contents of other processes of the same user.
- **Secrets** are not volumes. Delivering them is a separate concern, and no file content is
  logged.
