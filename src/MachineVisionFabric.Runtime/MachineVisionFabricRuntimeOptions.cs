using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Dataset;

namespace MachineVisionFabric.Runtime;

public sealed class MachineVisionFabricRuntimeOptions
{
    public const string SectionName = "MachineVisionFabric";

    public string IntegrationsRoot { get; set; } = "real-world-projects/integrations";

    public DatasetCaptureOptions DatasetCapture { get; set; } = new();

    public SimulatedPlcGateOptions PlcGate { get; set; } = new();
}
