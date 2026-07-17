using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Obsolete: The <c>runtime-builtin/dataset-writer</c> node type has been removed.
/// Use an <c>integration-module</c> node with <c>moduleId: "mvf.dataset-writer"</c> instead.
/// This file is kept to avoid breaking existing test references during migration.
/// </summary>
[Obsolete("Use FrameSinkNodeRunner with the mvf.dataset-writer integration module instead.")]
internal sealed class DatasetWriterNodeRunner(string nodeId) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        => Task.FromResult(NodeExecutionResult.NoOutput);

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
