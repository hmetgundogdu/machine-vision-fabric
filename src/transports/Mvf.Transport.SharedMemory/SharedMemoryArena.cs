using System.IO.MemoryMappedFiles;

namespace Mvf.Transport.SharedMemory;

/// <summary>
/// The engine-side data plane for co-located, out-of-process modules: a <b>file-backed</b>
/// shared-memory arena. A real file is memory-mapped here and <c>mmap</c>-ed by the child
/// (Python/Node/…), so both processes read/write the <b>same physical pages</b> — passing a frame
/// is passing a <see cref="FrameHandle"/> (offset+length), not copying bytes down a pipe. Local
/// only; no network.
///
/// <para>Slice A: a single size-class free-list. The frame originates on the .NET heap and is copied
/// into a slot <b>once</b>; the child reads it in place. Only .NET allocates here (the classifier's
/// output is a control signal over stdio), so the free-list lives in managed memory and needs no
/// cross-process coordination. Fan-out refcounts, module-requested allocation, and moving the
/// free-list into the shared segment arrive in later slices.</para>
///
/// <para>File-backed mappings of the same file share pages, so a write through this arena is visible
/// to the child's mapping without an explicit flush; the stdio round-trip that carries the handle
/// also orders the write before the child's read.</para>
/// </summary>
public sealed class SharedMemoryArena : IDisposable
{
    private readonly MemoryMappedFile _mmf;
    private readonly MemoryMappedViewAccessor _view;
    private readonly Stack<int> _freeSlots;
    private readonly object _gate = new();
    private readonly unsafe byte* _base;
    private bool _disposed;

    /// <summary>Absolute path of the backing file — hand this to the child so it can map the arena.</summary>
    public string FilePath { get; }

    public int SlotSize { get; }

    public int SlotCount { get; }

    public long Capacity => (long)SlotSize * SlotCount;

    public static SharedMemoryArena Create(SharedMemoryArenaOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);
        if (options.SlotSize <= 0 || options.SlotCount <= 0)
        {
            throw new ArgumentException("Arena slot size and count must both be positive.", nameof(options));
        }

        var path = Path.Combine(Path.GetTempPath(), $"mvf-arena-{Guid.NewGuid():N}.bin");
        var capacity = (long)options.SlotSize * options.SlotCount;

        // CreateNew pre-sizes the backing file (sparse on most filesystems until pages are touched).
        var mmf = MemoryMappedFile.CreateFromFile(
            path, FileMode.CreateNew, mapName: null, capacity, MemoryMappedFileAccess.ReadWrite);

        return new SharedMemoryArena(path, mmf, options.SlotSize, options.SlotCount);
    }

    private unsafe SharedMemoryArena(string filePath, MemoryMappedFile mmf, int slotSize, int slotCount)
    {
        FilePath = filePath;
        SlotSize = slotSize;
        SlotCount = slotCount;
        _mmf = mmf;
        _view = mmf.CreateViewAccessor(0, (long)slotSize * slotCount, MemoryMappedFileAccess.ReadWrite);

        byte* pointer = null;
        _view.SafeMemoryMappedViewHandle.AcquirePointer(ref pointer);
        _base = pointer;

        // Hand out low slots first for readable offsets while debugging.
        _freeSlots = new Stack<int>(Enumerable.Range(0, slotCount).Reverse());
    }

    /// <summary>
    /// Copies <paramref name="payload"/> into a free slot and returns a handle to it. Returns
    /// <c>false</c> — leaving the caller to fall back to inline transport — when the payload exceeds
    /// a slot or the arena is momentarily full.
    /// </summary>
    public bool TryWrite(ReadOnlySpan<byte> payload, out FrameHandle handle)
    {
        handle = default;
        if (payload.Length > SlotSize)
        {
            return false;
        }

        int slot;
        lock (_gate)
        {
            if (_disposed || _freeSlots.Count == 0)
            {
                return false;
            }

            slot = _freeSlots.Pop();
        }

        var offset = (long)slot * SlotSize;
        unsafe
        {
            payload.CopyTo(new Span<byte>(_base + offset, payload.Length));
        }

        handle = new FrameHandle(offset, payload.Length);
        return true;
    }

    /// <summary>Returns the slot backing <paramref name="handle"/> to the free-list.</summary>
    public void Release(FrameHandle handle)
    {
        var slot = (int)(handle.Offset / SlotSize);
        lock (_gate)
        {
            if (_disposed || slot < 0 || slot >= SlotCount)
            {
                return;
            }

            _freeSlots.Push(slot);
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
        }

        unsafe
        {
            if (_base != null)
            {
                _view.SafeMemoryMappedViewHandle.ReleasePointer();
            }
        }

        _view.Dispose();
        _mmf.Dispose();
        try
        {
            File.Delete(FilePath);
        }
        catch
        {
            // Best effort — the OS reclaims the temp file eventually.
        }
    }
}
