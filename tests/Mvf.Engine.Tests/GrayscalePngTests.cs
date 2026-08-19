using System.Buffers.Binary;
using System.IO.Compression;
using System.Text;
using Mvf.Cli.Imaging;

namespace Mvf.Engine.Tests;

/// <summary>
/// Pins the hand-rolled PNG writer. A binary format is exactly the kind of code that keeps working until
/// the day it silently does not — so these assertions read the produced bytes back the way a decoder
/// would, rather than trusting the encoder to describe itself.
/// </summary>
public sealed class GrayscalePngTests
{
    [Fact]
    public void ProducesAFileADecoderCanRead()
    {
        const int Width = 7;   // deliberately not a round number: the scanline stride is where this breaks
        const int Height = 5;

        var pixels = new byte[Width * Height];
        for (var i = 0; i < pixels.Length; i++)
        {
            pixels[i] = (byte)(i * 7);
        }

        var png = GrayscalePng.Encode(pixels, Width, Height);

        Assert.Equal(new byte[] { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A }, png[..8]);

        var chunks = ReadChunks(png);
        Assert.Equal(["IHDR", "IDAT", "IEND"], chunks.Select(c => c.Type));

        var header = chunks[0].Data;
        Assert.Equal((uint)Width, BinaryPrimitives.ReadUInt32BigEndian(header.AsSpan(0)));
        Assert.Equal((uint)Height, BinaryPrimitives.ReadUInt32BigEndian(header.AsSpan(4)));
        Assert.Equal(8, header[8]);  // bit depth
        Assert.Equal(0, header[9]);  // colour type 0 = grayscale
        Assert.Equal(0, header[12]); // not interlaced

        // Decompress with the framework's own reader: if our zlib stream or scanline layout were wrong,
        // this is where a real decoder would part company with us.
        var raw = Inflate(chunks[1].Data);
        Assert.Equal(Height * (Width + 1), raw.Length);

        for (var y = 0; y < Height; y++)
        {
            Assert.Equal(0, raw[y * (Width + 1)]); // every scanline declares filter 0
            Assert.Equal(
                pixels.AsSpan(y * Width, Width).ToArray(),
                raw.AsSpan((y * (Width + 1)) + 1, Width).ToArray());
        }
    }

    [Fact]
    public void EveryChunkCarriesAValidCrc()
    {
        var png = GrayscalePng.Encode(new byte[4 * 4], 4, 4);

        foreach (var (type, data, crc) in ReadChunks(png))
        {
            // Recomputed independently of the encoder's table, so a broken table cannot agree with itself.
            Assert.Equal(ReferenceCrc32(Encoding.ASCII.GetBytes(type).Concat(data).ToArray()), crc);
        }
    }

    [Fact]
    public void RejectsABufferThatDoesNotMatchTheDimensions()
    {
        // The guard that keeps a wrong descriptor from producing a corrupt file (the caller falls back to
        // saving raw bytes when this throws).
        Assert.Throws<ArgumentException>(() => GrayscalePng.Encode(new byte[10], 4, 4));
        Assert.Throws<ArgumentException>(() => GrayscalePng.Encode(new byte[0], 0, 4));
    }

    private static List<(string Type, byte[] Data, uint Crc)> ReadChunks(byte[] png)
    {
        var chunks = new List<(string, byte[], uint)>();
        var offset = 8;
        while (offset < png.Length)
        {
            var length = (int)BinaryPrimitives.ReadUInt32BigEndian(png.AsSpan(offset));
            var type = Encoding.ASCII.GetString(png, offset + 4, 4);
            var data = png[(offset + 8)..(offset + 8 + length)];
            var crc = BinaryPrimitives.ReadUInt32BigEndian(png.AsSpan(offset + 8 + length));
            chunks.Add((type, data, crc));
            offset += 12 + length;
        }

        return chunks;
    }

    private static byte[] Inflate(byte[] zlib)
    {
        using var input = new MemoryStream(zlib);
        using var decompressor = new ZLibStream(input, CompressionMode.Decompress);
        using var output = new MemoryStream();
        decompressor.CopyTo(output);
        return output.ToArray();
    }

    /// <summary>A plainly written CRC-32, kept separate from the encoder's table-driven one.</summary>
    private static uint ReferenceCrc32(byte[] bytes)
    {
        var crc = 0xFFFFFFFFu;
        foreach (var b in bytes)
        {
            crc ^= b;
            for (var i = 0; i < 8; i++)
            {
                crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xEDB88320u : crc >> 1;
            }
        }

        return crc ^ 0xFFFFFFFFu;
    }
}
