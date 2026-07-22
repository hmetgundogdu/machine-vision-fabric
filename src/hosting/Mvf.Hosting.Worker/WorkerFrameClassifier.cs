using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Processing;
using Mvf.Transport.SharedMemory;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IFrameClassifier"/> backed by an out-of-process worker (e.g. Python).
/// It drops into the existing FrameClassifierNodeRunner unchanged — the engine does not
/// know or care that the classifier runs in another language/process.
///
/// <para>When an <see cref="SharedMemoryArena"/> is supplied (M2), the frame is copied into the arena
/// once and only a <see cref="FrameHandle"/> travels over stdio; the worker reads the bytes in place.
/// Without an arena — or for a frame larger than a slot — it falls back to the inline base64 path
/// (M1), so the transport is a transparent optimization.</para>
/// </summary>
public sealed class WorkerFrameClassifier(StdioWorkerProcess worker, SharedMemoryArena? arena = null)
    : IFrameClassifier, IAsyncDisposable
{
    private int _requestId;

    public async Task<FrameClassification> ClassifyAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        byte[] bytes;
        await using (var stream = await frame.OpenReadAsync(cancellationToken))
        using (var buffer = new MemoryStream())
        {
            await stream.CopyToAsync(buffer, cancellationToken);
            bytes = buffer.ToArray();
        }

        var frameMessage = new JsonObject
        {
            ["cameraId"] = frame.CameraId,
            ["sequence"] = frame.SequenceNumber,
            ["contentType"] = frame.ContentType,
            ["length"] = bytes.Length,
        };

        // Prefer the shared-memory handle; fall back to inline base64 when there is no arena or the
        // frame does not fit a slot. The rented slot is held only for the duration of this one RPC.
        var handle = default(FrameHandle);
        var usedArena = arena is not null && arena.TryWrite(bytes, out handle);
        if (usedArena)
        {
            frameMessage["shm"] = new JsonObject
            {
                ["offset"] = handle.Offset,
                ["length"] = handle.Length,
            };
        }
        else
        {
            frameMessage["dataBase64"] = Convert.ToBase64String(bytes);
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
            if (usedArena)
            {
                arena!.Release(handle);
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

    public ValueTask DisposeAsync() => worker.DisposeAsync();
}
