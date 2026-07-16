using MachineVisionFabric.Contracts.Simulation;

namespace MachineVisionFabric.Contracts.Packages;

public sealed class FabricRuntimeProfile
{
    public string Name { get; set; } = "dataset-capture-starter";

    public string Mode { get; set; } = "dataset-collection-profile";

    public string Description { get; set; } = "Headless dataset collection profile.";

    public IReadOnlyList<string> Capabilities { get; set; } =
    [
        "dataset-capture",
        "simulator-source"
    ];

    public SourceBinding Source { get; set; } = new();

    public FolderSequenceSourceOptions SimulatorSource { get; set; } = new();
}
