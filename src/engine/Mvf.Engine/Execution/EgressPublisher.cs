using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution;

/// <summary>
/// Bridges a node's execution result to the realtime-egress sink. Lives in the engine (not core) because
/// only here are frame bytes + the <see cref="PayloadDescriptor"/> reachable — core's <see cref="IEgressSink"/>
/// takes them as raw buffers, so it never references <c>IFrameEnvelope</c>/<c>PayloadDescriptor</c>.
///
/// <para><b>Off the hot path.</b> The (bounded) frame read + copy happens only when the sink actually wants
/// frames (frame stream on <b>and</b> a subscriber attached) — an unwatched run does zero extra work. When
/// frames are wanted, the copy is the price of egress leaving the machine; it is opt-in via
/// <c>--egress-streams frame</c> and capped at <see cref="MaxPayloadBytes"/>.</para>
/// </summary>
internal static class EgressPublisher
{
    private const long MaxPayloadBytes = 32L * 1024 * 1024;

    public static void Publish(IEgressSink? sink, NodeExecutionEvent nodeEvent, NodeExecutionResult result)
    {
        if (sink is null)
        {
            return;
        }

        if (sink.WantsFrames
            && result.HasOutput
            && TryGetFrame(result, out var frame)
            && TryReadPayload(frame, out var descriptor, out var payload))
        {
            sink.PublishNodeTransitionFrame(nodeEvent, descriptor, payload);
        }
        else
        {
            sink.PublishNodeTransition(nodeEvent);
        }
    }

    private static bool TryGetFrame(NodeExecutionResult result, out IFrameEnvelope frame)
    {
        foreach (var (_, value) in result.All)
        {
            if (value.IsFrame && value.Frame is { } candidate)
            {
                frame = candidate;
                return true;
            }
        }

        frame = null!;
        return false;
    }

    private static bool TryReadPayload(
        IFrameEnvelope frame, out ReadOnlyMemory<byte> descriptor, out ReadOnlyMemory<byte> payload)
    {
        descriptor = default;
        payload = default;

        var length = frame.ContentLength ?? -1;
        if (length < 0 || length > MaxPayloadBytes)
        {
            return false;
        }

        byte[] bytes;
        try
        {
            using var stream = frame.OpenReadAsync(CancellationToken.None).AsTask().GetAwaiter().GetResult();
            bytes = new byte[length];
            ReadExact(stream, bytes);
        }
        catch
        {
            // Best-effort: a frame we cannot read is simply not streamed (the state record still goes out).
            return false;
        }

        var header = new byte[PayloadDescriptor.HeaderSize];
        DescriptorFor(frame, bytes.Length).WriteHeader(header);
        descriptor = header;
        payload = bytes;
        return true;
    }

    private static PayloadDescriptor DescriptorFor(IFrameEnvelope frame, int length)
    {
        // An arena frame already carries a typed descriptor; reuse it verbatim.
        if (frame is ArenaFrameEnvelope arena)
        {
            var existing = arena.Descriptor;
            if (existing.PayloadLength > 0)
            {
                return existing;
            }
        }

        // A frame that knows its own shape (a synthetic camera, a source that produces real images) is
        // taken at its word — but only when the shape actually accounts for the bytes, so a stale or wrong
        // descriptor cannot make a consumer read past the payload.
        if (frame.Descriptor is { } declared && DescribesExactly(declared, length))
        {
            return declared;
        }

        // Otherwise synthesize a byte-blob (or image) descriptor so the consumer still gets typed metadata.
        var media = frame.ContentType?.StartsWith("image/", StringComparison.OrdinalIgnoreCase) == true
            ? PayloadMediaType.Image
            : PayloadMediaType.Blob;
        return new PayloadDescriptor(media, PayloadElementType.UInt8, new long[] { length });
    }

    /// <summary>True when the descriptor's element count x element size is exactly the payload size.</summary>
    private static bool DescribesExactly(PayloadDescriptor descriptor, int length)
    {
        if (descriptor.Shape is not { Length: > 0 } shape)
        {
            return false;
        }

        var elements = 1L;
        foreach (var dimension in shape)
        {
            if (dimension <= 0)
            {
                return false;
            }

            elements *= dimension;
            if (elements > length)
            {
                return false; // overflow-safe: bail the moment it cannot fit
            }
        }

        return elements * ElementSize(descriptor.ElementType) == length;
    }

    private static int ElementSize(PayloadElementType type) => type switch
    {
        PayloadElementType.UInt8 or PayloadElementType.Int8 => 1,
        PayloadElementType.UInt16 or PayloadElementType.Int16
            or PayloadElementType.Float16 or PayloadElementType.BFloat16 => 2,
        PayloadElementType.UInt32 or PayloadElementType.Int32 or PayloadElementType.Float32 => 4,
        PayloadElementType.UInt64 or PayloadElementType.Int64 or PayloadElementType.Float64 => 8,
        _ => 0,
    };

    private static void ReadExact(Stream stream, byte[] buffer)
    {
        var offset = 0;
        while (offset < buffer.Length)
        {
            var read = stream.Read(buffer, offset, buffer.Length - offset);
            if (read == 0)
            {
                break;
            }

            offset += read;
        }
    }
}
