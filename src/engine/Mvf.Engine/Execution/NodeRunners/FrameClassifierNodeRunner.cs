using Mvf.Graph.Execution;
using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps an <see cref="IFrameClassifier"/> as a node that turns frame content into a
/// control signal — the first-class perception → control bridge. It reads a frame on
/// the data channel and emits a <c>classification</c> control signal that <c>switch</c>
/// (routes on ClassLabel) or <c>if</c> nodes act on, without forwarding the frame itself.
///
/// Input ports : <c>frame</c> (data)
/// Output ports: <c>class</c> (control)
/// </summary>
internal sealed class FrameClassifierNodeRunner(string nodeId, IFrameClassifier classifier) : INodeRunner, ICheckpointable
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    // Surfaces the wrapped classifier's checkpoint/restore (worker-backed classifiers are checkpointable)
    // so the executor can periodically snapshot it; a stateless or in-process classifier is a no-op.
    public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
        classifier is ICheckpointable checkpointable
            ? checkpointable.CheckpointAsync(cancellationToken)
            : Task.FromResult<byte[]?>(null);

    public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken) =>
        classifier is ICheckpointable checkpointable
            ? checkpointable.RestoreAsync(state, cancellationToken)
            : Task.CompletedTask;

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        if (frameInput?.Frame is null)
        {
            return NodeExecutionResult.NoOutput;
        }

        var classification = await classifier.ClassifyAsync(frameInput.Frame, cancellationToken);

        var signal = new ControlSignal
        {
            SignalType = "classification",
            Value = true,
            ClassLabel = classification.Label,
            Measurement = classification.Measurement,
            Unit = classification.Unit,
            Source = classification.Source,
            TimestampUtc = classification.EvaluatedAtUtc,
            Details = classification.Details
        };

        return NodeExecutionResult.Single("class", PortValue.FromControl(signal));
    }

    // The classifier may own external resources — an out-of-process worker (Python/Node)
    // holds a child process — so dispose it when it is disposable.
    public async ValueTask DisposeAsync()
    {
        switch (classifier)
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
