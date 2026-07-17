using MachineVisionFabric.Contracts.Pipelines;

namespace MachineVisionFabric.Core.Abstractions;

public sealed class ResolvedPipelineDefinition
{
    public required PipelineDefinition Definition { get; init; }

    public required string Source { get; init; }

    public required bool IsSynthetic { get; init; }
}
