using System.Buffers.Binary;
using Mvf.Abstractions;

namespace Mvf.Engine.Tests;

public sealed class PayloadDescriptorTests
{
    [Fact]
    public void WriteThenRead_RoundTripsShapeTypeAndLength()
    {
        var descriptor = new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [2, 3, 3], epoch: 7);
        Assert.Equal(18, descriptor.PayloadLength); // 2*3*3*1

        var header = new byte[PayloadDescriptor.HeaderSize];
        descriptor.WriteHeader(header);

        Assert.True(PayloadDescriptor.TryReadHeader(header, out var read));
        Assert.Equal(PayloadMediaType.Image, read.MediaType);
        Assert.Equal(PayloadElementType.UInt8, read.ElementType);
        Assert.Equal([2, 3, 3], read.Shape);
        Assert.Equal(7u, read.Epoch);
        Assert.Equal(18, read.PayloadLength);
    }

    [Fact]
    public void ComputeStrides_AreRowMajorInBytes()
    {
        var descriptor = new PayloadDescriptor(PayloadMediaType.Tensor, PayloadElementType.UInt8, [2, 3, 4]);
        Assert.Equal([12, 4, 1], descriptor.ComputeStrides());
    }

    [Fact]
    public void PayloadLength_ScalesWithElementSize()
    {
        var tensor = new PayloadDescriptor(PayloadMediaType.Tensor, PayloadElementType.Float32, [4]);
        Assert.Equal(16, tensor.PayloadLength); // 4 * 4 bytes
        Assert.Equal([4], tensor.ComputeStrides());
    }

    [Fact]
    public void HeaderSize_IsMultipleOf64_SoPayloadStaysAligned()
    {
        Assert.Equal(0, PayloadDescriptor.HeaderSize % 64);
    }

    [Fact]
    public void Constructor_RejectsRankAboveMax()
    {
        var tooManyDims = new long[PayloadDescriptor.MaxRank + 1];
        Array.Fill(tooManyDims, 1L);
        Assert.Throws<ArgumentException>(() => new PayloadDescriptor(PayloadMediaType.Tensor, PayloadElementType.UInt8, tooManyDims));
    }

    [Fact]
    public void TryReadHeader_RejectsBadMagic()
    {
        var header = new byte[PayloadDescriptor.HeaderSize];
        new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [1]).WriteHeader(header);
        BinaryPrimitives.WriteUInt32LittleEndian(header, 0xDEADBEEF);

        Assert.False(PayloadDescriptor.TryReadHeader(header, out _));
    }

    [Fact]
    public void TryReadHeader_RejectsRankAboveMax()
    {
        var header = new byte[PayloadDescriptor.HeaderSize];
        new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [1]).WriteHeader(header);
        header[15] = PayloadDescriptor.MaxRank + 1; // corrupt the rank byte

        Assert.False(PayloadDescriptor.TryReadHeader(header, out _));
    }

    [Fact]
    public void TryReadHeader_RejectsTamperedLength()
    {
        var header = new byte[PayloadDescriptor.HeaderSize];
        new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [2, 2]).WriteHeader(header);
        BinaryPrimitives.WriteInt64LittleEndian(header.AsSpan(16), 9999); // declared length no longer matches shape

        Assert.False(PayloadDescriptor.TryReadHeader(header, out _));
    }

    [Fact]
    public void TryValidate_RejectsPayloadExceedingSlot()
    {
        var descriptor = new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [1000]);

        Assert.False(descriptor.TryValidate(slotCapacity: 512, out var error));
        Assert.NotNull(error);
        Assert.True(descriptor.TryValidate(slotCapacity: PayloadDescriptor.HeaderSize + 1000, out _));
    }

    [Fact]
    public void TryValidate_RejectsSizeOverflow()
    {
        var descriptor = new PayloadDescriptor(PayloadMediaType.Tensor, PayloadElementType.Float64, [long.MaxValue, 2]);

        Assert.False(descriptor.TryValidate(slotCapacity: long.MaxValue, out var error));
        Assert.Contains("overflow", error, StringComparison.OrdinalIgnoreCase);
    }
}
