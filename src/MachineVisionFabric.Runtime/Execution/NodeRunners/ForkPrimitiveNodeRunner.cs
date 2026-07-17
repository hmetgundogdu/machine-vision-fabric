using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>fork</c> node runner.
/// Broadcasts the incoming data frame to all declared output ports unchanged.
/// Use this when the same frame needs to reach multiple downstream consumers
/// (e.g., disk writer + stream pusher + secondary classifier).
///
/// Input ports : <c>frame</c> (data)
/// Output ports: all ports declared in the node's <c>outputs</c> definition (any name)
/// </summary>
internal sealed class ForkPrimitiveNodeRunner(
    string nodeId,
    IReadOnlyList<string> outputPortNames) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        if (frameInput?.Frame is null || outputPortNames.Count == 0)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        var outputs = new Dictionary<string, PortValue>(outputPortNames.Count, StringComparer.OrdinalIgnoreCase);
        foreach (var port in outputPortNames)
        {
            outputs[port] = frameInput;
        }

        return Task.FromResult(new NodeExecutionResult(outputs));
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
