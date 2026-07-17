namespace MachineVisionFabric.Contracts.Packages;

public sealed class FabricProfileManifest
{
    public string Name { get; set; } = "dataset-capture-starter";

    public string Version { get; set; } = "0.1.0";

    public string EntryProfile { get; set; } = "profile.json";

    public string? PipelineDefinition { get; set; }

    public string Scenario { get; set; } = "dataset-capture";

    public DatasetCapturePolicy CapturePolicy { get; set; } = new();

    public ProductPresenceGateBinding ProductPresenceGate { get; set; } = new();

    public FrameProcessorBinding FrameProcessor { get; set; } = new();

    public IReadOnlyList<string> RequiredDirectories { get; set; } =
    [
        "assets",
        "assets/images",
        "assets/configs"
    ];
}
