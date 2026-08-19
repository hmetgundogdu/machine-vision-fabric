using System.Buffers.Binary;
using System.IO.Compression;
using System.Text;

namespace Mvf.Cli.Imaging;

/// <summary>
/// Writes single-channel 8-bit pixels as a PNG.
///
/// <para><b>Why encode at all:</b> a frame leaves the pipeline as raw pixels, which is right for compute —
/// but a raw buffer saved to disk is a file nothing opens. Handing the operator a PNG means the image can
/// be looked at with whatever the machine already has, mailed to whoever needs to see the defect, and
/// attached to a ticket. It also shrinks a 320x240 frame from 75 KB to a few.</para>
///
/// <para><b>Why hand-rolled:</b> PNG's grayscale case is small enough to write exactly — .NET supplies the
/// hard part (<see cref="ZLibStream"/> produces the zlib wrapper and its Adler-32), leaving the chunk
/// framing and a CRC-32. Taking an imaging dependency into an edge runtime that ships as a single file, for
/// one screen's save button, would cost far more than these few lines.</para>
///
/// <para>Lives in the CLI for now because the CLI is its only caller. When the studio preview stream needs
/// encoding on the egress path, that is the moment to move it somewhere shared — not before.</para>
/// </summary>
internal static class GrayscalePng
{
    private static readonly byte[] Signature = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    /// <summary>
    /// Encodes <paramref name="pixels"/> (row-major, one byte per pixel) as a grayscale PNG.
    /// </summary>
    /// <exception cref="ArgumentException">The buffer does not hold exactly width x height bytes.</exception>
    public static byte[] Encode(ReadOnlySpan<byte> pixels, int width, int height)
    {
        if (width <= 0 || height <= 0)
        {
            throw new ArgumentException("Width and height must be positive.");
        }

        if (pixels.Length != (long)width * height)
        {
            throw new ArgumentException(
                $"Expected {(long)width * height} pixels for {width}x{height}, got {pixels.Length}.", nameof(pixels));
        }

        using var output = new MemoryStream();
        output.Write(Signature);

        var header = new byte[13];
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(0), (uint)width);
        BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(4), (uint)height);
        header[8] = 8;  // bit depth
        header[9] = 0;  // colour type 0 = grayscale
        header[10] = 0; // deflate
        header[11] = 0; // adaptive filtering
        header[12] = 0; // no interlace
        WriteChunk(output, "IHDR", header);

        WriteChunk(output, "IDAT", Compress(pixels, width, height));
        WriteChunk(output, "IEND", []);

        return output.ToArray();
    }

    private static byte[] Compress(ReadOnlySpan<byte> pixels, int width, int height)
    {
        // Each scanline is prefixed with its filter type. Filter 0 (None) keeps this honest and simple:
        // the payload is already small and deflate does the compressing.
        var raw = new byte[(long)height * (width + 1) is var size && size <= int.MaxValue
            ? (int)size
            : throw new ArgumentException("Image too large to encode.")];

        for (var y = 0; y < height; y++)
        {
            var destination = y * (width + 1);
            raw[destination] = 0;
            pixels.Slice(y * width, width).CopyTo(raw.AsSpan(destination + 1));
        }

        using var compressed = new MemoryStream();
        using (var deflate = new ZLibStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            deflate.Write(raw);
        }

        return compressed.ToArray();
    }

    private static void WriteChunk(Stream output, string type, ReadOnlySpan<byte> data)
    {
        Span<byte> length = stackalloc byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(length, (uint)data.Length);
        output.Write(length);

        var typeBytes = Encoding.ASCII.GetBytes(type);
        output.Write(typeBytes);
        output.Write(data);

        // The CRC covers the type and the data, but not the length — per the PNG spec.
        var crc = Crc32(typeBytes, data);
        Span<byte> crcBytes = stackalloc byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(crcBytes, crc);
        output.Write(crcBytes);
    }

    private static readonly uint[] CrcTable = BuildCrcTable();

    private static uint[] BuildCrcTable()
    {
        var table = new uint[256];
        for (var n = 0; n < 256; n++)
        {
            var c = (uint)n;
            for (var k = 0; k < 8; k++)
            {
                c = (c & 1) != 0 ? 0xEDB88320u ^ (c >> 1) : c >> 1;
            }

            table[n] = c;
        }

        return table;
    }

    private static uint Crc32(ReadOnlySpan<byte> first, ReadOnlySpan<byte> second)
    {
        var c = 0xFFFFFFFFu;
        foreach (var b in first)
        {
            c = CrcTable[(c ^ b) & 0xFF] ^ (c >> 8);
        }

        foreach (var b in second)
        {
            c = CrcTable[(c ^ b) & 0xFF] ^ (c >> 8);
        }

        return c ^ 0xFFFFFFFFu;
    }
}
