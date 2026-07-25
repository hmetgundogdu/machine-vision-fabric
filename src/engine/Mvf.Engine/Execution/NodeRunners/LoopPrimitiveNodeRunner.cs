using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>loop</c> node runner: a portless, whole-graph marker.
///
/// <para>It carries no data and does no work in the cycle — every node in the graph runs every cycle, in
/// topological order, and the loop is not part of that dataflow. Its purpose is entirely declarative:
/// its <b>presence</b> tells the executor the run repeats and that a pause control exists.</para>
///
/// <para>Pause itself lives on the executor's <c>RunControl</c>, not here — the whole run has to stop
/// advancing, which no single node in the order can express. This runner therefore just exists and emits
/// nothing. Pause is not cancel: pausing stops the loop advancing with state kept and workers warm.</para>
///
/// Input ports : none
/// Output ports: none
/// </summary>
internal sealed class LoopPrimitiveNodeRunner(string nodeId) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
        Task.FromResult(NodeExecutionResult.NoOutput);

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
