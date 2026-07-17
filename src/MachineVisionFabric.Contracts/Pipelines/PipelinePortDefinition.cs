namespace MachineVisionFabric.Contracts.Pipelines;

public sealed class PipelinePortDefinition
{
    public string Name { get; set; } = "port";

    public string Channel { get; set; } = "data";

    public string DataType { get; set; } = "frame";

    public bool Required { get; set; } = true;

    public bool AllowMultipleEdges { get; set; }
}
