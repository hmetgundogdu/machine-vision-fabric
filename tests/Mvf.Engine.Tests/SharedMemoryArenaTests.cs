using System.IO.MemoryMappedFiles;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

public sealed class SharedMemoryArenaTests
{
    [Fact]
    public void TryWrite_ThenMapTheFile_ReadsBackTheSameBytes()
    {
        using var arena = SharedMemoryArena.Create(new SharedMemoryArenaOptions { SlotSize = 1024, SlotCount = 4 });
        var payload = new byte[] { 1, 2, 3, 4, 250, 251, 252 };

        Assert.True(arena.TryWrite(payload, out var handle));
        Assert.Equal(payload.Length, handle.Length);

        // A second, independent mapping of the same backing file — what the child process does —
        // must observe the bytes the arena wrote.
        using var mmf = MemoryMappedFile.CreateFromFile(
            arena.FilePath, FileMode.Open, mapName: null, arena.Capacity, MemoryMappedFileAccess.Read);
        using var accessor = mmf.CreateViewAccessor(0, arena.Capacity, MemoryMappedFileAccess.Read);

        var readBack = new byte[payload.Length];
        accessor.ReadArray(handle.Offset, readBack, 0, payload.Length);
        Assert.Equal(payload, readBack);
    }

    [Fact]
    public void TryWrite_WhenArenaFull_ReturnsFalseForFallback()
    {
        using var arena = SharedMemoryArena.Create(new SharedMemoryArenaOptions { SlotSize = 16, SlotCount = 1 });

        Assert.True(arena.TryWrite([1, 2, 3], out var first));
        Assert.False(arena.TryWrite([4, 5, 6], out _)); // no free slot → caller falls back to base64

        arena.Release(first);
        Assert.True(arena.TryWrite([7, 8, 9], out _)); // slot reclaimed
    }

    [Fact]
    public void TryWrite_WhenPayloadExceedsSlot_ReturnsFalse()
    {
        using var arena = SharedMemoryArena.Create(new SharedMemoryArenaOptions { SlotSize = 8, SlotCount = 2 });

        Assert.False(arena.TryWrite(new byte[9], out _));
    }

    [Fact]
    public void Release_ReturnsSlotToFreeList_SoCapacityIsBounded()
    {
        using var arena = SharedMemoryArena.Create(new SharedMemoryArenaOptions { SlotSize = 8, SlotCount = 2 });

        // Rent/return in a loop many more times than there are slots: each RPC releases before the
        // next rents, so two slots suffice indefinitely.
        for (var i = 0; i < 100; i++)
        {
            Assert.True(arena.TryWrite([(byte)i], out var handle));
            arena.Release(handle);
        }
    }
}
