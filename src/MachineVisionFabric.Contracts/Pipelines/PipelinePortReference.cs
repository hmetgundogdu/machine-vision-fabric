namespace MachineVisionFabric.Contracts.Pipelines;

public sealed class PipelinePortReference
{
    public string NodeId { get; set; } = "node";

    public string Port { get; set; } = "port";
}
