using MachineVisionFabric.Contracts.Pipelines;

namespace MachineVisionFabric.Core.Abstractions;

public interface IPipelineDefinitionValidator
{
    PipelineValidationResult Validate(PipelineDefinition definition);
}
