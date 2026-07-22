using System.IO.MemoryMappedFiles;
using Mvf.Abstractions;

namespace Mvf.Transport.SharedMemory;

/// <summary>
/// The engine's <see cref="IDataPlane"/> for co-located, out-of-process modules: a <b>file-backed</b>
/// shared-memory arena. A real file is memory-mapped here and <c>mmap</c>-ed by the child
/// (Python/Node/…), so both processes read/write the <b>same physical pages</b> — passing a payload is
/// passing an <see cref="ArenaHandle"/> (offset+length), not copying bytes down a pipe. Local only;
/// no network.
///
/// <para>Single size-class free-list with a per-slot reference count: a payload is published with a
/// count equal to its number of consumers (known from the static graph); each consumer releases and
/// the slot returns to the free-list at zero. Only .NET allocates here, so the free-list lives in
/// managed memory and needs no cross-process coordination; a worker only ever reads a handed-in
/// handle. The backing file is created lazily on first use, so a run with no workers pays nothing.</para>
///
/// <para>File-backed mappings of the same file share pages, so a write here is visible to the child's
/// mapping without an explicit flush; the stdio round-trip that carries the handle also orders the
/// write before the child's read. Payloads are opaque bytes — the type comes from the typed graph.</para>
/// </summary>
public sealed class SharedMemoryArena : IDataPlane, IDisposable
{
    private readonly SharedMemoryArenaOptions _options;
    private readonly Lock _gate = new();

    private MemoryMappedFile? _mmf;
    private MemoryMappedViewAccessor? _view;
    private Stack<int>? _freeSlots;
    private int[]? _refCounts;
    private unsafe byte* _base;
    private string? _backingPath;
    private bool _disposed;

    public SharedMemoryArena(SharedMemoryArenaOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);
        if (options.SlotSize <= 0 || options.SlotCount <= 0)
        {
            throw new ArgumentException("Arena slot size and count must both be positive.", nameof(options));
        }

        _options = options;
    }

    public SharedMemoryArena() : this(new SharedMemoryArenaOptions())
    {
    }

    public int SlotSize => _options.SlotSize;

    public int SlotCount => _options.SlotCount;

    public long Capacity => (long)_options.SlotSize * _options.SlotCount;

    /// <summary>Absolute path of the backing file — hand this to the child so it can map the arena.</summary>
    public string BackingPath
    {
        get
        {
            lock (_gate)
            {
                EnsureInitialized();
                return _backingPath!;
            }
        }
    }

    public bool TryPublish(in PayloadDescriptor descriptor, ReadOnlySpan<byte> payload, int referenceCount, out ArenaHandle handle)
    {
        handle = default;
        if (referenceCount < 1)
        {
            throw new ArgumentOutOfRangeException(nameof(referenceCount), "A published payload needs at least one consumer.");
        }

        // The descriptor must describe exactly these bytes, and header + payload must fit a slot.
        if (descriptor.PayloadLength != payload.Length
            || PayloadDescriptor.HeaderSize + (long)payload.Length > _options.SlotSize)
        {
            return false;
        }

        int slot;
        lock (_gate)
        {
            EnsureInitialized();
            if (_disposed || _freeSlots!.Count == 0)
            {
                return false;
            }

            slot = _freeSlots.Pop();
            _refCounts![slot] = referenceCount;
        }

        var offset = (long)slot * _options.SlotSize;
        unsafe
        {
            var slotSpan = new Span<byte>(_base + offset, _options.SlotSize);
            descriptor.WriteHeader(slotSpan);
            payload.CopyTo(slotSpan[PayloadDescriptor.HeaderSize..]);
        }

        handle = new ArenaHandle(offset, payload.Length);
        return true;
    }

    public bool TryReadDescriptor(ArenaHandle handle, out PayloadDescriptor descriptor)
    {
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (_mmf is null)
            {
                descriptor = default;
                return false;
            }

            unsafe
            {
                var header = new ReadOnlySpan<byte>(_base + handle.Offset, PayloadDescriptor.HeaderSize);
                return PayloadDescriptor.TryReadHeader(header, out descriptor);
            }
        }
    }

    public Stream OpenRead(ArenaHandle handle)
    {
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            if (_mmf is null)
            {
                throw new InvalidOperationException("Cannot read from an arena that has not been initialized.");
            }

            // Expose only the payload — past the descriptor header.
            unsafe
            {
                return new UnmanagedMemoryStream(
                    _base + handle.Offset + PayloadDescriptor.HeaderSize, handle.Length, handle.Length, FileAccess.Read);
            }
        }
    }

    public bool TryReserve(out ArenaHandle handle)
    {
        handle = default;
        int slot;
        lock (_gate)
        {
            EnsureInitialized();
            if (_disposed || _freeSlots!.Count == 0)
            {
                return false;
            }

            slot = _freeSlots.Pop();
            _refCounts![slot] = 1; // producer hold
        }

        var offset = (long)slot * _options.SlotSize;
        handle = new ArenaHandle(offset, _options.SlotSize - PayloadDescriptor.HeaderSize);
        return true;
    }

    public void AddRef(ArenaHandle handle, int count)
    {
        if (count <= 0)
        {
            return;
        }

        var slot = (int)(handle.Offset / _options.SlotSize);
        lock (_gate)
        {
            if (_disposed || _refCounts is null || slot < 0 || slot >= _options.SlotCount)
            {
                return;
            }

            // Only grow a live slot; a reclaimed (zero) slot must not be resurrected.
            if (_refCounts[slot] > 0)
            {
                _refCounts[slot] += count;
            }
        }
    }

    public void Release(ArenaHandle handle)
    {
        var slot = (int)(handle.Offset / _options.SlotSize);
        lock (_gate)
        {
            if (_disposed || _refCounts is null || slot < 0 || slot >= _options.SlotCount)
            {
                return;
            }

            if (_refCounts[slot] > 0 && --_refCounts[slot] == 0)
            {
                _freeSlots!.Push(slot);
            }
        }
    }

    private void EnsureInitialized()
    {
        if (_mmf is not null)
        {
            return;
        }

        ObjectDisposedException.ThrowIf(_disposed, this);

        var path = Path.Combine(Path.GetTempPath(), $"mvf-arena-{Guid.NewGuid():N}.bin");

        // CreateNew pre-sizes the backing file (sparse on most filesystems until pages are touched).
        var mmf = MemoryMappedFile.CreateFromFile(
            path, FileMode.CreateNew, mapName: null, Capacity, MemoryMappedFileAccess.ReadWrite);
        var view = mmf.CreateViewAccessor(0, Capacity, MemoryMappedFileAccess.ReadWrite);

        unsafe
        {
            byte* pointer = null;
            view.SafeMemoryMappedViewHandle.AcquirePointer(ref pointer);
            _base = pointer;
        }

        _mmf = mmf;
        _view = view;
        _backingPath = path;
        _refCounts = new int[_options.SlotCount];
        // Hand out low slots first for readable offsets while debugging.
        _freeSlots = new Stack<int>(Enumerable.Range(0, _options.SlotCount).Reverse());
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

            if (_view is not null)
            {
                unsafe
                {
                    if (_base != default)
                    {
                        _view.SafeMemoryMappedViewHandle.ReleasePointer();
                        _base = default;
                    }
                }

                _view.Dispose();
            }

            _mmf?.Dispose();

            if (_backingPath is not null)
            {
                try
                {
                    File.Delete(_backingPath);
                }
                catch
                {
                    // Best effort — the OS reclaims the temp file eventually.
                }
            }
        }
    }
}
