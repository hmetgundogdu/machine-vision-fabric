using Mvf.Graph.Execution;
using Mvf.Graph.Processing;
using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps an <see cref="IFrameProcessor"/> as a compute node.
/// Passes the incoming frame through when the processor accepts it.
///
/// Input ports : <c>frame</c> (data)
/// Output ports: <c>frame</c> (data) — emitted only when processor accepts
/// </summary>
internal sealed class FrameProcessorNodeRunner(string nodeId, IFrameProcessor processor) : INodeRunner
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

        var decision = await processor.EvaluateAsync(frameInput.Frame, cancellationToken);
        if (!decision.Accepted)
        {
            return NodeExecutionResult.NoOutput;
        }

        return NodeExecutionResult.Single("frame", frameInput);
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
