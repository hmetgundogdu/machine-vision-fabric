namespace MachineVisionFabric.Contracts.Simulation;

public sealed class FolderSequenceSourceOptions
{
    public string SourceFolder { get; set; } = ".\\examples\\packages\\dataset-capture-starter\\assets\\images";

    public bool Loop { get; set; } = true;

    public int FrameIntervalMs { get; set; } = 250;

    public int ParallelCameraCount { get; set; } = 1;
}
