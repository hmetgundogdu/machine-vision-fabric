using System.Buffers.Binary;
using System.IO.MemoryMappedFiles;

namespace Mvf.Worker;

/// <summary>
/// The shared-memory arena at MVF_ARENA_PATH, mapped once. Slots are
/// <c>[192-byte descriptor | payload bytes]</c> (docs/data-plane-design.md).
/// </summary>
internal sealed unsafe class Arena : IDisposable
{
    private const uint Magic = 0x3146564Du; // 'MVF1'
    private const ushort Version = 1;
    private const int HeaderSize = 192;
    private const int MaxRank = 8;

    private readonly MemoryMappedFile _file;
    private readonly MemoryMappedViewAccessor _view;
    private readonly byte* _base;
    private readonly long _size;

    public Arena(string path, bool writable)
    {
        var access = writable ? MemoryMappedFileAccess.ReadWrite : MemoryMappedFileAccess.Read;
        var stream = new FileStream(path, FileMode.Open,
            writable ? FileAccess.ReadWrite : FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        _file = MemoryMappedFile.CreateFromFile(stream, null, 0, access, HandleInheritability.None,
            leaveOpen: false);
        _view = _file.CreateViewAccessor(0, 0, access);
        byte* p = null;
        _view.SafeMemoryMappedViewHandle.AcquirePointer(ref p);
        _base = p + _view.PointerOffset;
        _size = _view.Capacity;
    }

    public void Dispose()
    {
        _view.SafeMemoryMappedViewHandle.ReleasePointer();
        _view.Dispose();
        _file.Dispose();
    }

    /// <summary>Reads the descriptor at <paramref name="offset"/> into <paramref name="into"/>.</summary>
    public void Read(long offset, Payload into)
    {
        if (offset < 0)
        {
            throw new InvalidOperationException("frame is missing its shared-memory handle");
        }

        Check(offset, HeaderSize);
        var h = new ReadOnlySpan<byte>(_base + offset, HeaderSize);
        if (BinaryPrimitives.ReadUInt32LittleEndian(h) != Magic ||
            BinaryPrimitives.ReadUInt16LittleEndian(h[4..]) != Version)
        {
            throw new InvalidDataException("bad payload header (magic/version)");
        }

        int rank = h[15];
        if (rank > MaxRank)
        {
            throw new InvalidDataException("payload rank exceeds max");
        }

        long length = BinaryPrimitives.ReadInt64LittleEndian(h[16..]);
        Check(offset + HeaderSize, length);
        into.MediaType = (MediaType)BinaryPrimitives.ReadUInt16LittleEndian(h[12..]);
        into.ElementType = (ElementType)h[14];
        if (into.Shape.Length != rank)
        {
            into.Shape = new long[rank];
        }

        for (var i = 0; i < rank; i++)
        {
            into.Shape[i] = BinaryPrimitives.ReadInt64LittleEndian(h[(24 + i * 8)..]);
        }

        into.Pointer = _base + offset + HeaderSize;
        into.Length = checked((int)length);
    }

    /// <summary>Writes <paramref name="output"/> into the slot at <paramref name="offset"/>.</summary>
    public void Write(long offset, Output output, long capacity)
    {
        var rank = output.Shape.Length;
        if (rank > MaxRank)
        {
            throw new InvalidOperationException("output rank exceeds max");
        }

        long elementSize = ElementSize(output.ElementType);
        var length = elementSize;
        foreach (var dim in output.Shape)
        {
            length *= dim;
        }

        if (length != output.Data.Length)
        {
            throw new InvalidOperationException("output payload size disagrees with its descriptor");
        }

        if (length > capacity)
        {
            throw new InvalidOperationException("output payload exceeds the reserved slot capacity");
        }

        Check(offset, HeaderSize + length);
        var h = new Span<byte>(_base + offset, HeaderSize);
        BinaryPrimitives.WriteUInt32LittleEndian(h, Magic);
        BinaryPrimitives.WriteUInt16LittleEndian(h[4..], Version);
        h[6] = 1; // C-contiguous
        BinaryPrimitives.WriteUInt32LittleEndian(h[8..], 0); // epoch
        BinaryPrimitives.WriteUInt16LittleEndian(h[12..], (ushort)output.MediaType);
        h[14] = (byte)output.ElementType;
        h[15] = (byte)rank;
        BinaryPrimitives.WriteInt64LittleEndian(h[16..], length);
        var stride = elementSize;
        for (var i = rank - 1; i >= 0; i--)
        {
            BinaryPrimitives.WriteInt64LittleEndian(h[(24 + i * 8)..], output.Shape[i]);
            BinaryPrimitives.WriteInt64LittleEndian(h[(88 + i * 8)..], stride);
            stride *= output.Shape[i];
        }

        output.Data.Span.CopyTo(new Span<byte>(_base + offset + HeaderSize, (int)length));
    }

    private void Check(long offset, long length)
    {
        if (offset < 0 || length < 0 || offset + length > _size)
        {
            throw new InvalidDataException($"slot {offset}+{length} lies outside the arena ({_size} bytes)");
        }
    }

    private static long ElementSize(ElementType type) => type switch
    {
        ElementType.UInt8 or ElementType.Int8 => 1,
        ElementType.UInt16 or ElementType.Int16 or ElementType.Float16 or ElementType.BFloat16 => 2,
        ElementType.UInt32 or ElementType.Int32 or ElementType.Float32 => 4,
        ElementType.UInt64 or ElementType.Int64 or ElementType.Float64 => 8,
        _ => throw new InvalidOperationException("unknown element type"),
    };
}
