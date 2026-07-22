using System.Buffers.Binary;

namespace Mvf.Abstractions;

/// <summary>Element (scalar) type of a typed payload — DLPack/numpy-compatible.</summary>
public enum PayloadElementType : byte
{
    UInt8 = 0, Int8 = 1,
    UInt16 = 2, Int16 = 3,
    UInt32 = 4, Int32 = 5,
    UInt64 = 6, Int64 = 7,
    Float16 = 8, BFloat16 = 9, Float32 = 10, Float64 = 11,
}

/// <summary>Semantic tag for a payload. Numeric data is a <see cref="Tensor"/>; an image is a tensor
/// with this tag; structured text is <see cref="Json"/>; anything else is a <see cref="Blob"/>.</summary>
public enum PayloadMediaType : ushort
{
    Blob = 0, Tensor = 1, Image = 2, Json = 3,
}

/// <summary>
/// The self-describing, byte-based descriptor for a payload in the shared data plane. It carries just
/// enough (dtype + shape, DLPack-style) for a consumer to wrap the raw bytes as a **native object with
/// zero copy** — a numpy array, a .NET <see cref="Span{T}"/>, a tensor. It lives in the slot header
/// (co-located with the bytes), is fixed little-endian binary (no JSON on the hot path), and is
/// **validated by the engine at every boundary** — a module's header is never trusted blindly.
///
/// <para>v1 is <b>C-contiguous only</b>: strides are derived (row-major) and written for the consumer's
/// convenience, but non-contiguous layouts are rejected so bounds are trivially provable and the
/// zero-copy wrap is unconditional.</para>
/// </summary>
public readonly struct PayloadDescriptor
{
    /// <summary>'MVF1' as a little-endian u32.</summary>
    public const uint Magic = 0x3146564D;
    public const ushort CurrentVersion = 1;
    public const int MaxRank = 8;

    /// <summary>Fixed header size; a multiple of 64 so the payload that follows stays 64-byte aligned.</summary>
    public const int HeaderSize = 192;

    private const byte FlagContiguous = 0x01;

    // Field offsets within the header.
    private const int OffsetMagic = 0;      // u32
    private const int OffsetVersion = 4;    // u16
    private const int OffsetFlags = 6;      // u8
    private const int OffsetEpoch = 8;      // u32
    private const int OffsetMediaType = 12; // u16
    private const int OffsetElementType = 14; // u8
    private const int OffsetRank = 15;      // u8
    private const int OffsetPayloadLength = 16; // i64
    private const int OffsetShape = 24;     // i64[MaxRank]
    private const int OffsetStrides = OffsetShape + MaxRank * 8; // i64[MaxRank] → 88

    public PayloadDescriptor(PayloadMediaType mediaType, PayloadElementType elementType, ReadOnlySpan<long> shape, uint epoch = 0)
    {
        if (shape.Length > MaxRank)
        {
            throw new ArgumentException($"Rank {shape.Length} exceeds the maximum of {MaxRank}.", nameof(shape));
        }

        foreach (var dimension in shape)
        {
            if (dimension < 0)
            {
                throw new ArgumentException("Shape dimensions must be non-negative.", nameof(shape));
            }
        }

        MediaType = mediaType;
        ElementType = elementType;
        Shape = shape.ToArray();
        Epoch = epoch;
    }

    public PayloadMediaType MediaType { get; }

    public PayloadElementType ElementType { get; }

    public long[] Shape { get; }

    public uint Epoch { get; }

    public int Rank => Shape?.Length ?? 0;

    public int ElementSize => SizeOf(ElementType);

    /// <summary>Total payload bytes for a C-contiguous buffer. Throws <see cref="OverflowException"/> on overflow.</summary>
    public long PayloadLength
    {
        get
        {
            long count = 1;
            foreach (var dimension in Shape ?? [])
            {
                count = checked(count * dimension);
            }

            return checked(count * ElementSize);
        }
    }

    public static int SizeOf(PayloadElementType elementType) => elementType switch
    {
        PayloadElementType.UInt8 or PayloadElementType.Int8 => 1,
        PayloadElementType.UInt16 or PayloadElementType.Int16 or PayloadElementType.Float16 or PayloadElementType.BFloat16 => 2,
        PayloadElementType.UInt32 or PayloadElementType.Int32 or PayloadElementType.Float32 => 4,
        PayloadElementType.UInt64 or PayloadElementType.Int64 or PayloadElementType.Float64 => 8,
        _ => throw new ArgumentOutOfRangeException(nameof(elementType), elementType, "Unknown element type."),
    };

    /// <summary>Row-major (C-order) byte strides.</summary>
    public long[] ComputeStrides()
    {
        var strides = new long[Rank];
        long running = ElementSize;
        for (var i = Rank - 1; i >= 0; i--)
        {
            strides[i] = running;
            running = checked(running * Shape[i]);
        }

        return strides;
    }

    /// <summary>Writes the fixed header into <paramref name="destination"/> (which must be ≥ <see cref="HeaderSize"/>).</summary>
    public void WriteHeader(Span<byte> destination)
    {
        if (destination.Length < HeaderSize)
        {
            throw new ArgumentException($"Header destination must be at least {HeaderSize} bytes.", nameof(destination));
        }

        var header = destination[..HeaderSize];
        header.Clear();

        BinaryPrimitives.WriteUInt32LittleEndian(header[OffsetMagic..], Magic);
        BinaryPrimitives.WriteUInt16LittleEndian(header[OffsetVersion..], CurrentVersion);
        header[OffsetFlags] = FlagContiguous;
        BinaryPrimitives.WriteUInt32LittleEndian(header[OffsetEpoch..], Epoch);
        BinaryPrimitives.WriteUInt16LittleEndian(header[OffsetMediaType..], (ushort)MediaType);
        header[OffsetElementType] = (byte)ElementType;
        header[OffsetRank] = (byte)Rank;
        BinaryPrimitives.WriteInt64LittleEndian(header[OffsetPayloadLength..], PayloadLength);

        var strides = ComputeStrides();
        for (var i = 0; i < Rank; i++)
        {
            BinaryPrimitives.WriteInt64LittleEndian(header[(OffsetShape + i * 8)..], Shape[i]);
            BinaryPrimitives.WriteInt64LittleEndian(header[(OffsetStrides + i * 8)..], strides[i]);
        }
    }

    /// <summary>
    /// Reads and structurally validates a header. Returns false — never throws — for a bad magic/version,
    /// an out-of-range rank, an unknown element type, or a declared length that disagrees with the shape.
    /// </summary>
    public static bool TryReadHeader(ReadOnlySpan<byte> source, out PayloadDescriptor descriptor)
    {
        descriptor = default;
        if (source.Length < HeaderSize)
        {
            return false;
        }

        if (BinaryPrimitives.ReadUInt32LittleEndian(source[OffsetMagic..]) != Magic
            || BinaryPrimitives.ReadUInt16LittleEndian(source[OffsetVersion..]) != CurrentVersion)
        {
            return false;
        }

        int rank = source[OffsetRank];
        if (rank > MaxRank)
        {
            return false;
        }

        var elementType = (PayloadElementType)source[OffsetElementType];
        if (!Enum.IsDefined(elementType))
        {
            return false;
        }

        var mediaType = (PayloadMediaType)BinaryPrimitives.ReadUInt16LittleEndian(source[OffsetMediaType..]);
        var epoch = BinaryPrimitives.ReadUInt32LittleEndian(source[OffsetEpoch..]);

        var shape = new long[rank];
        for (var i = 0; i < rank; i++)
        {
            shape[i] = BinaryPrimitives.ReadInt64LittleEndian(source[(OffsetShape + i * 8)..]);
            if (shape[i] < 0)
            {
                return false;
            }
        }

        PayloadDescriptor candidate;
        long declaredLength;
        try
        {
            candidate = new PayloadDescriptor(mediaType, elementType, shape, epoch);
            declaredLength = BinaryPrimitives.ReadInt64LittleEndian(source[OffsetPayloadLength..]);
            if (declaredLength != candidate.PayloadLength)
            {
                return false;
            }
        }
        catch (Exception ex) when (ex is ArgumentException or OverflowException)
        {
            return false;
        }

        descriptor = candidate;
        return true;
    }

    /// <summary>
    /// Validates that this payload fits a slot of <paramref name="slotCapacity"/> bytes (header + payload)
    /// and that its size arithmetic does not overflow. Security boundary check — call before exposing a
    /// module-written buffer to a consumer.
    /// </summary>
    public bool TryValidate(long slotCapacity, out string? error)
    {
        error = null;

        if (!Enum.IsDefined(MediaType))
        {
            error = $"Unknown media type {(ushort)MediaType}.";
            return false;
        }

        long length;
        try
        {
            length = PayloadLength;
        }
        catch (OverflowException)
        {
            error = "Payload size arithmetic overflowed.";
            return false;
        }

        if (HeaderSize + length > slotCapacity)
        {
            error = $"Payload of {length} bytes (+{HeaderSize} header) exceeds the slot capacity of {slotCapacity}.";
            return false;
        }

        return true;
    }
}
