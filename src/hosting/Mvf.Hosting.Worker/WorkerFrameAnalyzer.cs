using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Processing;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IFrameAnalyzer"/> backed by an out-of-process worker (e.g. Python): the worker reads
/// the input frame from the arena, may write a derived frame back into a reserved output slot, and may
/// also return classification and structured inference metadata over the control plane.
/// </summary>
public sealed class WorkerFrameAnalyzer(IWorkerChannel worker, IDataPlane dataPlane)
    : IFrameAnalyzer, ICheckpointable, IWorkerMetricsSource, IReconfigurable, IAsyncDisposable
{
    private readonly WorkerCallMetrics _metrics = new();
    private int _requestId;

    public WorkerMetricsSnapshot GetWorkerMetrics() => _metrics.Snapshot(worker);

    public bool CanReconfigure => worker.SupportsConfigure;

    public async Task<bool> TryReconfigureAsync(JsonNode? config, CancellationToken cancellationToken)
    {
        if (!worker.SupportsConfigure)
        {
            return false;
        }

        await worker.ConfigureAsync(config, cancellationToken);
        return true;
    }

    public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
        worker is ICheckpointable checkpointable
            ? checkpointable.CheckpointAsync(cancellationToken)
            : WorkerCheckpoint.CheckpointAsync(worker, dataPlane, ++_requestId, cancellationToken);

    public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken) =>
        worker is ICheckpointable checkpointable
            ? checkpointable.RestoreAsync(state, cancellationToken)
            : WorkerCheckpoint.RestoreAsync(worker, dataPlane, ++_requestId, state, cancellationToken);

    public async Task<FrameAnalysisResult> AnalyzeAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        var frameMessage = new JsonObject();
        var ownInputHandle = await WorkerFrameMarshal.AttachInputAsync(frameMessage, frame, dataPlane, cancellationToken);

        if (!dataPlane.TryReserve(out var outputHandle))
        {
            if (ownInputHandle is { } inputToRelease)
            {
                dataPlane.Release(inputToRelease);
            }

            throw new InvalidOperationException("The data plane has no free slot for the analyzer's output.");
        }

        var request = new JsonObject
        {
            ["type"] = "execute",
            ["id"] = ++_requestId,
            ["frame"] = frameMessage,
            ["out"] = new JsonObject
            {
                ["offset"] = outputHandle.Offset,
                ["capacity"] = outputHandle.Length,
            },
        };

        JsonObject response;
        var startedAt = WorkerCallMetrics.Start();
        var failed = true;
        try
        {
            response = await worker.RequestAsync(request, cancellationToken);
            failed = (string?)response["type"] == "error";
        }
        catch
        {
            dataPlane.Release(outputHandle);
            throw;
        }
        finally
        {
            _metrics.Complete(startedAt, failed);
            if (ownInputHandle is { } inputToRelease)
            {
                dataPlane.Release(inputToRelease);
            }
        }

        if ((string?)response["type"] == "error")
        {
            dataPlane.Release(outputHandle);
            throw new InvalidOperationException($"Worker analyze failed: {(string?)response["message"]}");
        }

        IFrameEnvelope? outputFrame = null;
        if (response["frame"] is JsonObject)
        {
            if (!dataPlane.TryReadDescriptor(outputHandle, out var descriptor)
                || !descriptor.TryValidate(dataPlane.SlotSize, out _))
            {
                dataPlane.Release(outputHandle);
                throw new InvalidOperationException("Analyzer output has an invalid or oversized descriptor.");
            }

            outputFrame = new ArenaFrameEnvelope(
                dataPlane,
                new ArenaHandle(outputHandle.Offset, (int)descriptor.PayloadLength),
                frame);
        }
        else
        {
            dataPlane.Release(outputHandle);
        }

        FrameClassification? classification = null;
        if (response["classification"] is JsonObject rawClassification)
        {
            classification = new FrameClassification(
                Label: (string?)rawClassification["label"] ?? "unknown",
                Source: $"worker:{worker.ModuleId}",
                EvaluatedAtUtc: DateTime.UtcNow,
                Measurement: (double?)rawClassification["measurement"],
                Unit: (string?)rawClassification["unit"],
                Details: (string?)rawClassification["details"]);
        }

        return new FrameAnalysisResult(
            Frame: outputFrame,
            Classification: classification,
            Result: response["value"]?.DeepClone());
    }

    public ValueTask DisposeAsync() => worker.DisposeAsync();
}
