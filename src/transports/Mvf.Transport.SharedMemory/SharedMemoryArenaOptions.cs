namespace Mvf.Transport.SharedMemory;

/// <summary>
/// Sizing for a <see cref="SharedMemoryArena"/>. Slice A uses a single size-class: the arena is
/// <see cref="SlotCount"/> fixed slots of <see cref="SlotSize"/> bytes each. Segregated size-classes
/// arrive in a later slice.
/// </summary>
public sealed class SharedMemoryArenaOptions
{
    /// <summary>Bytes per slot — must be at least the largest frame payload you expect.</summary>
    public int SlotSize { get; init; } = 8 * 1024 * 1024;

    /// <summary>Number of slots. Total arena bytes = <see cref="SlotSize"/> × <see cref="SlotCount"/>.</summary>
    public int SlotCount { get; init; } = 8;
}
