using Mvf.Graph.Dataset;

namespace Mvf.Engine;

public sealed class MachineVisionFabricRuntimeOptions
{
    public const string SectionName = "MachineVisionFabric";

    public string IntegrationsRoot { get; set; } = "modules";

    public DatasetCaptureOptions DatasetCapture { get; set; } = new();
}
