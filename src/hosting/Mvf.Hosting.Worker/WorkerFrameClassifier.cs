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
public sealed class WorkerFrameClassifier(IWorkerChannel worker, IDataPlane dataPlane)
    : IFrameClassifier, ICheckpointable, IAsyncDisposable
{
    private int _requestId;

    // A supervised channel owns checkpoint/restore (it must hold the last state to recover with);
    // a plain channel falls back to a one-shot capture/restore.
    public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
        worker is ICheckpointable checkpointable
            ? checkpointable.CheckpointAsync(cancellationToken)
            : WorkerCheckpoint.CheckpointAsync(worker, dataPlane, ++_requestId, cancellationToken);

    public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken) =>
        worker is ICheckpointable checkpointable
            ? checkpointable.RestoreAsync(state, cancellationToken)
            : WorkerCheckpoint.RestoreAsync(worker, dataPlane, ++_requestId, state, cancellationToken);

    public async Task<FrameClassification> ClassifyAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        var frameMessage = new JsonObject();
        var ownHandle = await WorkerFrameMarshal.AttachInputAsync(frameMessage, frame, dataPlane, cancellationToken);

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
            if (ownHandle is { } handle)
            {
                dataPlane.Release(handle);
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
