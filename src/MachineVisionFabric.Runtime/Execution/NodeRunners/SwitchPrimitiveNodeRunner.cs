using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>switch</c> node runner.
/// Routes the incoming data frame to the output port whose name matches the
/// string value of the <c>class</c> control signal.
/// If no port matches, routes to <c>default</c> if declared; otherwise emits NoOutput.
///
/// Input ports : <c>frame</c> (data), <c>class</c> (control — string signal value)
/// Output ports: one port per routing target, plus optional <c>default</c>
///
/// Example:
///   class signal = "accept"  →  frame emitted on output port "accept"
///   class signal = "reject"  →  frame emitted on output port "reject"
///   class signal = "unknown" →  frame emitted on output port "default" (if declared)
/// </summary>
internal sealed class SwitchPrimitiveNodeRunner(
    string nodeId,
    IReadOnlySet<string> outputPortNames) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        var classInput = inputs.Get("class");

        if (frameInput?.Frame is null || classInput?.Control is null)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        var targetPort = classInput.Control.ClassLabel ?? string.Empty;

        // Try exact match first, then fall through to "default"
        if (outputPortNames.Contains(targetPort))
        {
            return Task.FromResult(NodeExecutionResult.Single(targetPort, frameInput));
        }

        if (outputPortNames.Contains("default"))
        {
            return Task.FromResult(NodeExecutionResult.Single("default", frameInput));
        }

        return Task.FromResult(NodeExecutionResult.NoOutput);
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
