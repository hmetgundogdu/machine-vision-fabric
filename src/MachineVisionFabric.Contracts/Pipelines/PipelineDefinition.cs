namespace MachineVisionFabric.Contracts.Pipelines;

public sealed class PipelineDefinition
{
    public string Name { get; set; } = "pipeline";

    public string Version { get; set; } = "0.1.0";

    public string Description { get; set; } = "Typed pipeline graph definition.";

    public string RuntimeMode { get; set; } = "headless-dataset-collection";

    public IReadOnlyList<string> Capabilities { get; set; } = [];

    public IReadOnlyList<PipelineNodeDefinition> Nodes { get; set; } = [];

    public IReadOnlyList<PipelineEdgeDefinition> Edges { get; set; } = [];
}
