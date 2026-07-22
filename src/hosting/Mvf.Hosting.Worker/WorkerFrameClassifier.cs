using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Processing;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IFrameClassifier"/> backed by an out-of-process worker (e.g. Python).
/// It drops into the existing FrameClassifierNodeRunner unchanged — the engine does not
/// know or care that the classifier runs in another language/process.
/// </summary>
public sealed class WorkerFrameClassifier(StdioWorkerProcess worker) : IFrameClassifier, IAsyncDisposable
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

        var request = new JsonObject
        {
            ["type"] = "execute",
            ["id"] = ++_requestId,
            ["frame"] = new JsonObject
            {
                ["cameraId"] = frame.CameraId,
                ["sequence"] = frame.SequenceNumber,
                ["contentType"] = frame.ContentType,
                // M1: frame carried inline over the local pipe. M2 replaces this with a
                // shared-memory handle (no copy).
                ["dataBase64"] = Convert.ToBase64String(bytes),
            },
        };

        var response = await worker.RequestAsync(request, cancellationToken);
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
