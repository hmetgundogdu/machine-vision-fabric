namespace Mvf.Transport.SharedMemory;

/// <summary>
/// A reference to a payload living in the shared-memory arena: a byte <see cref="Offset"/> from the
/// arena base plus the payload <see cref="Length"/>. It is an <b>offset, never a pointer</b> — each
/// process maps the arena at a different base address, so only offsets are portable across the
/// language boundary. Small enough to travel in a control message.
/// </summary>
public readonly record struct FrameHandle(long Offset, int Length);
