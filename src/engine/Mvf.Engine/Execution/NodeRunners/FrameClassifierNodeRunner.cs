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
internal sealed class FrameClassifierNodeRunner(string nodeId, IFrameClassifier classifier) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

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

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
