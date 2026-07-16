namespace MachineVisionFabric.Contracts.Packages;

public sealed class DatasetCapturePolicy
{
    public bool Enabled { get; set; } = true;

    public bool RequireProductPresent { get; set; } = true;

    public int? MaxFramesPerCamera { get; set; }

    public int PreTriggerFramesPerCamera { get; set; }

    public int PostTriggerFramesPerCamera { get; set; }

    public int GateEvaluationIntervalFrames { get; set; } = 1;

    public bool IncludeSourcePathInMetadata { get; set; } = true;

    public string Mode { get; set; } = "full-stream";
}
