using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>if</c> node runner.
/// Gate semantics: passes the incoming data frame through only when
/// the boolean-gate control signal on <c>productPresent</c> is true.
///
/// Input ports : <c>frame</c> (data), <c>productPresent</c> (control)
/// Output ports: <c>acceptedFrame</c> (data) — emitted only when gate is open
/// </summary>
internal sealed class IfPrimitiveNodeRunner(string nodeId) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        var controlInput = inputs.Get("productPresent");

        if (frameInput?.Frame is null || controlInput?.Control is null)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        if (!controlInput.Control.Value)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        return Task.FromResult(NodeExecutionResult.Single("acceptedFrame", frameInput));
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
