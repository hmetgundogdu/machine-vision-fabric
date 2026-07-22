using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;

namespace Mvf.Hosting.Worker;

/// <summary>
/// Shared frame marshalling for worker runners: a frame always reaches the child as a shared-memory
/// handle (no base64). If the engine already published it (an <see cref="ArenaFrameEnvelope"/>), the
/// handle is forwarded and the engine owns the lifetime; otherwise it is published for the single RPC
/// and the returned handle must be released afterwards.
/// </summary>
internal static class WorkerFrameMarshal
{
    /// <summary>
    /// Fills <paramref name="frameMessage"/> with the frame metadata and its <c>shm</c> handle. Returns a
    /// handle to <see cref="IDataPlane.Release"/> after the RPC when this call published it, or null when
    /// the frame was already in the arena (engine-owned).
    /// </summary>
    public static async Task<ArenaHandle?> AttachInputAsync(
        JsonObject frameMessage,
        IFrameEnvelope frame,
        IDataPlane dataPlane,
        CancellationToken cancellationToken)
    {
        frameMessage["cameraId"] = frame.CameraId;
        frameMessage["sequence"] = frame.SequenceNumber;
        frameMessage["contentType"] = frame.ContentType;

        if (frame is ArenaFrameEnvelope arenaFrame)
        {
            // The child reads the typed descriptor from the slot header at this offset.
            frameMessage["shm"] = ShmOffset(arenaFrame.Handle);
            return null;
        }

        byte[] bytes;
        await using (var stream = await frame.OpenReadAsync(cancellationToken))
        using (var buffer = new MemoryStream())
        {
            await stream.CopyToAsync(buffer, cancellationToken);
            bytes = buffer.ToArray();
        }

        // An encoded frame is an opaque byte blob (u8). No base64 fallback — a failure is a hard error.
        var descriptor = new PayloadDescriptor(PayloadMediaType.Blob, PayloadElementType.UInt8, [bytes.Length]);
        if (!dataPlane.TryPublish(descriptor, bytes, referenceCount: 1, out var handle))
        {
            throw new InvalidOperationException(
                $"Frame of {bytes.Length} bytes could not be published to the data plane (slot capacity {dataPlane.SlotSize}).");
        }

        frameMessage["shm"] = ShmOffset(handle);
        return handle;
    }

    public static JsonObject ShmOffset(ArenaHandle handle) => new()
    {
        ["offset"] = handle.Offset,
    };
}
