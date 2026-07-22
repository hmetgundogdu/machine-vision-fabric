using System.IO.MemoryMappedFiles;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

public sealed class SharedMemoryArenaTests
{
    [Fact]
    public void Publish_ThenMapTheFile_ReadsBackTheSameBytes()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 1024, SlotCount = 4 });
        var payload = new byte[] { 1, 2, 3, 4, 250, 251, 252 };

        Assert.True(arena.TryPublish(payload, referenceCount: 1, out var handle));
        Assert.Equal(payload.Length, handle.Length);

        // A second, independent mapping of the same backing file — what the child process does —
        // must observe the bytes the arena wrote.
        using var mmf = MemoryMappedFile.CreateFromFile(
            arena.BackingPath, FileMode.Open, mapName: null, arena.Capacity, MemoryMappedFileAccess.Read);
        using var accessor = mmf.CreateViewAccessor(0, arena.Capacity, MemoryMappedFileAccess.Read);

        var readBack = new byte[payload.Length];
        accessor.ReadArray(handle.Offset, readBack, 0, payload.Length);
        Assert.Equal(payload, readBack);
    }

    [Fact]
    public void OpenRead_ReturnsAZeroCopyStreamOverThePayload()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 64, SlotCount = 2 });
        var payload = new byte[] { 9, 8, 7, 6, 5 };
        Assert.True(arena.TryPublish(payload, referenceCount: 1, out var handle));

        using var stream = arena.OpenRead(handle);
        var buffer = new byte[payload.Length];
        stream.ReadExactly(buffer);

        Assert.Equal(payload, buffer);
        Assert.Equal(payload.Length, stream.Length);
    }

    [Fact]
    public void Release_ReclaimsSlotOnlyWhenRefcountHitsZero()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 16, SlotCount = 1 });

        // Publish to the single slot with two consumers (fan-out).
        Assert.True(arena.TryPublish([1, 2, 3], referenceCount: 2, out var handle));
        Assert.False(arena.TryPublish([4], referenceCount: 1, out _)); // slot still held

        arena.Release(handle); // one consumer done, refcount 2 → 1
        Assert.False(arena.TryPublish([4], referenceCount: 1, out _)); // still held

        arena.Release(handle); // last consumer done, refcount 1 → 0 → reclaimed
        Assert.True(arena.TryPublish([4], referenceCount: 1, out _));
    }

    [Fact]
    public void TryPublish_WhenArenaFull_ReturnsFalseForFallback()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 16, SlotCount = 1 });

        Assert.True(arena.TryPublish([1, 2, 3], referenceCount: 1, out var first));
        Assert.False(arena.TryPublish([4, 5, 6], referenceCount: 1, out _)); // no free slot → caller falls back

        arena.Release(first);
        Assert.True(arena.TryPublish([7, 8, 9], referenceCount: 1, out _)); // slot reclaimed
    }

    [Fact]
    public void TryPublish_WhenPayloadExceedsSlot_ReturnsFalse()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 8, SlotCount = 2 });

        Assert.False(arena.TryPublish(new byte[9], referenceCount: 1, out _));
    }
}
