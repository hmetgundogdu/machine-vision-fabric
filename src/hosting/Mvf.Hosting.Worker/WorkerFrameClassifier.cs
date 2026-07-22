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
/// <para>The frame reaches the worker as a shared-memory handle (M2) rather than base64 whenever it
/// can. If the engine already published it (an <see cref="ArenaFrameEnvelope"/> — e.g. one copy fanned
/// out to several workers), its handle is forwarded as-is and the engine owns the lifetime. Otherwise
/// this classifier publishes it for the single RPC, or falls back to inline base64 when there is no
/// <see cref="IDataPlane"/> or the frame does not fit a slot.</para>
/// </summary>
public sealed class WorkerFrameClassifier(StdioWorkerProcess worker, IDataPlane? dataPlane = null)
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
            // The graph is typed; data/frame is the only payload today. Sending it keeps the wire
            // self-describing as tensors and other data types join the data plane later.
            ["dataType"] = "data/frame",
        };

        var ownHandle = default(ArenaHandle);
        var releaseOwn = false;

        if (frame is ArenaFrameEnvelope arenaFrame)
        {
            // Already in the arena (engine-published): forward the handle, no copy, no release.
            frameMessage["length"] = arenaFrame.Handle.Length;
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

            frameMessage["length"] = bytes.Length;

            // Publish for this one RPC (single consumer → refcount 1), or fall back to inline base64.
            if (dataPlane is not null && dataPlane.TryPublish(bytes, referenceCount: 1, out ownHandle))
            {
                releaseOwn = true;
                frameMessage["shm"] = ShmHandle(ownHandle);
            }
            else
            {
                frameMessage["dataBase64"] = Convert.ToBase64String(bytes);
            }
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
                dataPlane!.Release(ownHandle);
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

    private static JsonObject ShmHandle(ArenaHandle handle) => new()
    {
        ["offset"] = handle.Offset,
        ["length"] = handle.Length,
    };

    public ValueTask DisposeAsync() => worker.DisposeAsync();
}
