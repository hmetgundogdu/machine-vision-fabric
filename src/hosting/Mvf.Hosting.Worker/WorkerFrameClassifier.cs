using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Processing;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IFrameClassifier"/> backed by an out-of-process worker (e.g. Python).
/// It drops into the existing FrameClassifierNodeRunner unchanged — the engine does not
/// know or care that the classifier runs in another language/process.
///
/// <para>The frame always reaches the worker as a shared-memory handle — there is no base64 path. If
/// the engine already published it (an <see cref="ArenaFrameEnvelope"/> — e.g. one copy fanned out to
/// several workers), its handle is forwarded as-is and the engine owns the lifetime. Otherwise this
/// classifier publishes it into the data plane for the single RPC and releases it afterwards.</para>
/// </summary>
public sealed class WorkerFrameClassifier(StdioWorkerProcess worker, IDataPlane dataPlane)
    : IFrameClassifier, IAsyncDisposable
{
    private int _requestId;

    public async Task<FrameClassification> ClassifyAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        var frameMessage = new JsonObject
        {
            ["cameraId"] = frame.CameraId,
            ["sequence"] = frame.SequenceNumber,
            ["contentType"] = frame.ContentType,
        };

        var ownHandle = default(ArenaHandle);
        var releaseOwn = false;

        if (frame is ArenaFrameEnvelope arenaFrame)
        {
            // Already in the arena (engine-published): forward the handle, no copy, no release. The
            // child reads the typed descriptor from the slot header at this offset.
            frameMessage["shm"] = ShmHandle(arenaFrame.Handle);
        }
        else
        {
            byte[] bytes;
            await using (var stream = await frame.OpenReadAsync(cancellationToken))
            using (var buffer = new MemoryStream())
            {
                await stream.CopyToAsync(buffer, cancellationToken);
                bytes = buffer.ToArray();
            }

            // An encoded frame is an opaque byte blob (u8). Publish for this one RPC (refcount 1); no
            // base64 fallback — a failure to publish is a hard error.
            var descriptor = new PayloadDescriptor(PayloadMediaType.Blob, PayloadElementType.UInt8, [bytes.Length]);
            if (!dataPlane.TryPublish(descriptor, bytes, referenceCount: 1, out ownHandle))
            {
                throw new InvalidOperationException(
                    $"Frame of {bytes.Length} bytes could not be published to the data plane (slot capacity {dataPlane.SlotSize}).");
            }

            releaseOwn = true;
            frameMessage["shm"] = ShmHandle(ownHandle);
        }

        var request = new JsonObject
        {
            ["type"] = "execute",
            ["id"] = ++_requestId,
            ["frame"] = frameMessage,
        };

        JsonObject response;
        try
        {
            response = await worker.RequestAsync(request, cancellationToken);
        }
        finally
        {
            if (releaseOwn)
            {
                dataPlane.Release(ownHandle);
            }
        }

        if ((string?)response["type"] == "error")
        {
            throw new InvalidOperationException($"Worker classify failed: {(string?)response["message"]}");
        }

        var classification = response["classification"] as JsonObject
            ?? throw new InvalidOperationException("Worker result is missing 'classification'.");

        return new FrameClassification(
            Label: (string?)classification["label"] ?? "unknown",
            Source: $"worker:{worker.ModuleId}",
            EvaluatedAtUtc: DateTime.UtcNow,
            Measurement: (double?)classification["measurement"],
            Unit: (string?)classification["unit"],
            Details: (string?)classification["details"]);
    }

    // Only the offset travels; the child reads the typed descriptor (media type, dtype, shape, length)
    // from the slot header at this offset — the single cross-language source of truth.
    private static JsonObject ShmHandle(ArenaHandle handle) => new()
    {
        ["offset"] = handle.Offset,
    };

    public ValueTask DisposeAsync() => worker.DisposeAsync();
}
