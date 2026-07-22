namespace Mvf.Abstractions;

/// <summary>
/// A reference to a payload living in the shared-memory data plane: a byte <see cref="Offset"/> from
/// the arena base plus the payload <see cref="Length"/>. It is an <b>offset, never a pointer</b> —
/// each process maps the arena at a different base address, so only offsets are portable across the
/// language boundary. Payload-agnostic: the <i>type</i> of what lives there comes from the static
/// typed graph (data/frame today, data/tensor later), not from the handle.
/// </summary>
public readonly record struct ArenaHandle(long Offset, int Length);

/// <summary>
/// The engine-owned data plane for co-located, out-of-process modules: an arena of shared memory that
/// carries a **typed payload** — a <see cref="PayloadDescriptor"/> header plus opaque bytes — by
/// <see cref="ArenaHandle"/> instead of copying it down a pipe. A consumer wraps the bytes as a native
/// object with zero copy using the descriptor; the plane itself never parses the bytes. There is no
/// base64/inline fallback — payloads always travel through the arena. The engine references only this
/// seam; the concrete (file-backed) arena lives in a transport project the core never references.
/// Local only; no network.
///
/// <para>Ownership follows the graph: a payload is published <b>once</b> with a reference count equal
/// to its number of consumers (known statically); each consumer <see cref="Release"/>s when done and
/// the slot is reclaimed at zero. The free-list and refcount table live here (engine side) — a worker
/// only ever reads a handed-in handle.</para>
/// </summary>
public interface IDataPlane
{
    /// <summary>
    /// Backing-file path of the arena (created lazily on first use). Hand this to a co-located worker
    /// so it can map the same memory and read payloads by handle.
    /// </summary>
    string BackingPath { get; }

    /// <summary>Maximum bytes (descriptor header + payload) that fit one slot.</summary>
    int SlotSize { get; }

    /// <summary>
    /// Writes <paramref name="descriptor"/> (as the slot header) followed by <paramref name="payload"/>
    /// into a free slot, with an initial <paramref name="referenceCount"/> (= its number of consumers),
    /// and returns a handle. Returns <c>false</c> when the payload does not fit a slot, the arena is
    /// momentarily full, or the payload length disagrees with the descriptor. There is no fallback — the
    /// caller must treat <c>false</c> as an error.
    /// </summary>
    bool TryPublish(in PayloadDescriptor descriptor, ReadOnlySpan<byte> payload, int referenceCount, out ArenaHandle handle);

    /// <summary>Reads the typed descriptor from the slot header for <paramref name="handle"/>.</summary>
    bool TryReadDescriptor(ArenaHandle handle, out PayloadDescriptor descriptor);

    /// <summary>Opens a zero-copy, read-only stream over the <b>payload</b> bytes (past the header).</summary>
    Stream OpenRead(ArenaHandle handle);

    /// <summary>Decrements the reference count for <paramref name="handle"/>; reclaims the slot at zero.</summary>
    void Release(ArenaHandle handle);
}
