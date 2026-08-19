using System.Buffers.Binary;

namespace Mvf.Egress;

/// <summary>
/// Reads length-prefixed egress records off a byte stream (a TCP connection, a WebSocket binary channel
/// adapted to a stream, a file). The consumer-side counterpart to <see cref="TcpServerEgressSink"/>;
/// reused by the .NET consumer SDK and the tests.
/// </summary>
public sealed class EgressStreamReader(Stream stream)
{
    private readonly byte[] _lengthBuffer = new byte[4];

    /// <summary>Reads the next record, or null at end of stream.</summary>
    public async Task<DecodedEgressRecord?> ReadAsync(CancellationToken cancellationToken = default)
    {
        if (!await ReadExactAsync(_lengthBuffer, cancellationToken).ConfigureAwait(false))
        {
            return null;
        }

        var length = BinaryPrimitives.ReadUInt32LittleEndian(_lengthBuffer);
        var body = new byte[length];
        if (!await ReadExactAsync(body, cancellationToken).ConfigureAwait(false))
        {
            return null;
        }

        return EgressWire.Decode(body);
    }

    private async Task<bool> ReadExactAsync(byte[] buffer, CancellationToken cancellationToken)
    {
        var offset = 0;
        while (offset < buffer.Length)
        {
            var read = await stream.ReadAsync(buffer.AsMemory(offset), cancellationToken).ConfigureAwait(false);
            if (read == 0)
            {
                return false;
            }

            offset += read;
        }

        return true;
    }
}
