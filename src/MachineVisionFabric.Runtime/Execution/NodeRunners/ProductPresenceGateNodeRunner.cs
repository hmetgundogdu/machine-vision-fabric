using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Evaluates an <see cref="IProductPresenceGate"/> each cycle and emits
/// a control signal on the <c>productPresent</c> output port.
/// </summary>
internal sealed class ProductPresenceGateNodeRunner(string nodeId, IProductPresenceGate gate) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var decision = await gate.EvaluateAsync(cancellationToken);

        var signal = new ControlSignal
        {
            SignalType = "boolean-gate",
            Value = decision.ProductPresent,
            Source = decision.Source,
            StationId = decision.StationId,
            TimestampUtc = decision.EvaluatedAtUtc,
            Details = decision.Details
        };

        return NodeExecutionResult.Single("productPresent", PortValue.FromControl(signal));
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
