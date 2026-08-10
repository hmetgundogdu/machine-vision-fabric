using System.IO.MemoryMappedFiles;
using Mvf.Abstractions;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

public sealed class SharedMemoryArenaTests
{
    private static PayloadDescriptor Blob(int length) =>
        new(PayloadMediaType.Blob, PayloadElementType.UInt8, [length]);

    [Fact]
    public void Publish_WritesHeaderThenPayload_AndMappingReadsBothBack()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 4 });
        var payload = new byte[] { 1, 2, 3, 4, 250, 251, 252 };

        Assert.True(arena.TryPublish(Blob(payload.Length), payload, referenceCount: 1, out var handle));

        // The engine-side typed read.
        Assert.True(arena.TryReadDescriptor(handle, out var descriptor));
        Assert.Equal(PayloadMediaType.Blob, descriptor.MediaType);
        Assert.Equal([payload.Length], descriptor.Shape);

        // A second, independent mapping of the same file — what the child does — sees a valid header at
        // the handle offset and the payload right after it.
        //
        // Opened through a shared FileStream rather than by path: the path overload asks for exclusive
        // access, which on Windows is refused because the arena that created the file still holds it.
        // A real child does not hit this (it opens the file shared), so the strict overload only ever
        // failed the test, never the product.
        using var backing = new FileStream(
            arena.BackingPath, FileMode.Open, FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete);
        using var mmf = MemoryMappedFile.CreateFromFile(
            backing, mapName: null, arena.Capacity, MemoryMappedFileAccess.Read,
            HandleInheritability.None, leaveOpen: true);
        using var accessor = mmf.CreateViewAccessor(0, arena.Capacity, MemoryMappedFileAccess.Read);

        var header = new byte[PayloadDescriptor.HeaderSize];
        accessor.ReadArray(handle.Offset, header, 0, header.Length);
        Assert.True(PayloadDescriptor.TryReadHeader(header, out _));

        var readBack = new byte[payload.Length];
        accessor.ReadArray(handle.Offset + PayloadDescriptor.HeaderSize, readBack, 0, payload.Length);
        Assert.Equal(payload, readBack);
    }

    [Fact]
    public void OpenRead_ReturnsAZeroCopyStreamOverThePayloadOnly()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 2 });
        var payload = new byte[] { 9, 8, 7, 6, 5 };
        Assert.True(arena.TryPublish(Blob(payload.Length), payload, referenceCount: 1, out var handle));

        using var stream = arena.OpenRead(handle);
        var buffer = new byte[payload.Length];
        stream.ReadExactly(buffer);

        Assert.Equal(payload, buffer);
        Assert.Equal(payload.Length, stream.Length); // payload only, not header + payload
    }

    [Fact]
    public void Release_ReclaimsSlotOnlyWhenRefcountHitsZero()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 512, SlotCount = 1 });

        Assert.True(arena.TryPublish(Blob(3), [1, 2, 3], referenceCount: 2, out var handle)); // fan-out to two
        Assert.False(arena.TryPublish(Blob(1), [4], referenceCount: 1, out _));                // slot still held

        arena.Release(handle); // one consumer done, refcount 2 → 1
        Assert.False(arena.TryPublish(Blob(1), [4], referenceCount: 1, out _));

        arena.Release(handle); // last consumer done, refcount 1 → 0 → reclaimed
        Assert.True(arena.TryPublish(Blob(1), [4], referenceCount: 1, out _));
    }

    [Fact]
    public void TryPublish_WhenArenaFull_ReturnsFalse()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 512, SlotCount = 1 });

        Assert.True(arena.TryPublish(Blob(3), [1, 2, 3], referenceCount: 1, out var first));
        Assert.False(arena.TryPublish(Blob(3), [4, 5, 6], referenceCount: 1, out _));

        arena.Release(first);
        Assert.True(arena.TryPublish(Blob(3), [7, 8, 9], referenceCount: 1, out _));
    }

    [Fact]
    public void TryPublish_WhenHeaderPlusPayloadExceedsSlot_ReturnsFalse()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 256, SlotCount = 2 });

        // 256 - 192 header = 64 bytes fit; 100 does not.
        Assert.True(arena.TryPublish(Blob(64), new byte[64], referenceCount: 1, out _));
        Assert.False(arena.TryPublish(Blob(100), new byte[100], referenceCount: 1, out _));
    }

    [Fact]
    public void TryPublish_WhenDescriptorLengthDisagreesWithPayload_ReturnsFalse()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 512, SlotCount = 1 });

        Assert.False(arena.TryPublish(Blob(10), new byte[7], referenceCount: 1, out _));
    }

    [Fact]
    public void Reserve_AddRef_Release_BalanceReclaimsSlotOnlyAtZero()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 1 });

        Assert.True(arena.TryReserve(out var handle)); // producer hold → refcount 1
        arena.AddRef(handle, 2);                        // now 3 (two more consumer edges)

        arena.Release(handle);                          // 2
        arena.Release(handle);                          // 1
        Assert.False(arena.TryPublish(Blob(1), [1], referenceCount: 1, out _)); // still held

        arena.Release(handle);                          // 0 → reclaimed
        Assert.True(arena.TryPublish(Blob(1), [1], referenceCount: 1, out _));
    }

    [Fact]
    public void TryReserve_HandsBackTheSlotPayloadCapacity()
    {
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 1 });

        Assert.True(arena.TryReserve(out var handle));
        Assert.Equal(4096 - PayloadDescriptor.HeaderSize, handle.Length);
    }
}
