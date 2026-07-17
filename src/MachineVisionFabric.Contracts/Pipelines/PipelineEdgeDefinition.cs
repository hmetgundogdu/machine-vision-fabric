namespace MachineVisionFabric.Contracts.Pipelines;

public sealed class PipelineEdgeDefinition
{
    public string Id { get; set; } = "edge";

    public string Kind { get; set; } = "data";

    public PipelinePortReference From { get; set; } = new();

    public PipelinePortReference To { get; set; } = new();

    public string? Condition { get; set; }
}
