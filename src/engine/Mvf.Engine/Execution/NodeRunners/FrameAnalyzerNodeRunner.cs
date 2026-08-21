using System.Text.Json.Nodes;
using Mvf.Graph.Execution;
using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps an <see cref="IFrameAnalyzer"/> as a node that may emit a derived frame, a routing
/// classification, and structured inference metadata from one input frame.
///
/// Input ports : <c>frame</c> (data)
/// Output ports: <c>frame</c> (data), <c>class</c> (control), <c>result</c> (control/value:json)
/// </summary>
internal sealed class FrameAnalyzerNodeRunner(string nodeId, IFrameAnalyzer analyzer)
    : INodeRunner, ICheckpointable, IWorkerMetricsSource, IReconfigurable
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public bool CanReconfigure => (analyzer as IReconfigurable)?.CanReconfigure ?? false;

    public Task<bool> TryReconfigureAsync(JsonNode? config, CancellationToken cancellationToken) =>
        analyzer is IReconfigurable reconfigurable
            ? reconfigurable.TryReconfigureAsync(config, cancellationToken)
            : Task.FromResult(false);

    public WorkerMetricsSnapshot? GetWorkerMetrics() =>
        (analyzer as IWorkerMetricsSource)?.GetWorkerMetrics();

    public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
        analyzer is ICheckpointable checkpointable
            ? checkpointable.CheckpointAsync(cancellationToken)
            : Task.FromResult<byte[]?>(null);

    public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken) =>
        analyzer is ICheckpointable checkpointable
            ? checkpointable.RestoreAsync(state, cancellationToken)
            : Task.CompletedTask;

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        if (frameInput?.Frame is null)
        {
            return NodeExecutionResult.NoOutput;
        }

        var analysis = await analyzer.AnalyzeAsync(frameInput.Frame, cancellationToken);
        var pairs = new List<(string portName, PortValue value)>(3);

        if (analysis.Frame is not null)
        {
            pairs.Add(("frame", PortValue.FromFrame(analysis.Frame)));
        }

        if (analysis.Classification is not null)
        {
            pairs.Add(("class", PortValue.FromControl(new ControlSignal
            {
                SignalType = "classification",
                Value = true,
                ClassLabel = analysis.Classification.Label,
                Measurement = analysis.Classification.Measurement,
                Unit = analysis.Classification.Unit,
                Source = analysis.Classification.Source,
                TimestampUtc = analysis.Classification.EvaluatedAtUtc,
                Details = analysis.Classification.Details
            })));
        }

        if (analysis.Result is not null)
        {
            pairs.Add(("result", PortValue.FromControl(new ControlSignal
            {
                SignalType = "value",
                Value = true,
                Payload = analysis.Result,
                Source = analysis.Classification?.Source ?? string.Empty,
                TimestampUtc = analysis.Classification?.EvaluatedAtUtc ?? DateTime.UtcNow
            })));
        }

        return pairs.Count == 0
            ? NodeExecutionResult.NoOutput
            : NodeExecutionResult.FromPairs([.. pairs]);
    }

    public async ValueTask DisposeAsync()
    {
        switch (analyzer)
        {
            case IAsyncDisposable asyncDisposable:
                await asyncDisposable.DisposeAsync();
                break;
            case IDisposable disposable:
                disposable.Dispose();
                break;
        }
    }
}
