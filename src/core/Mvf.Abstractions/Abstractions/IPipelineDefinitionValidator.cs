using Mvf.Graph.Pipelines;

namespace Mvf.Abstractions;

public interface IPipelineDefinitionValidator
{
    PipelineValidationResult Validate(PipelineDefinition definition);
}
