using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Abstractions;

/// <summary>
/// Resolves a <see cref="PipelineNodeDefinition"/> into an activated <see cref="INodeRunner"/>.
/// Each node kind (integration-module, embedded-primitive, runtime-builtin) has its own resolution strategy.
/// </summary>
public interface IPipelineNodeActivator
{
    /// <summary>
    /// Creates and activates a runner for the given node.
    /// </summary>
    Task<INodeRunner> ActivateAsync(
        PipelineNodeDefinition node,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken);
}
