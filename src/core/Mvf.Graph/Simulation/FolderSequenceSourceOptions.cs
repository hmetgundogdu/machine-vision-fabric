namespace Mvf.Graph.Simulation;

public sealed class FolderSequenceSourceOptions
{
    // Relative paths are resolved against the package root at activation time.
    public string SourceFolder { get; set; } = "assets/images";

    public bool Loop { get; set; } = true;

    public int FrameIntervalMs { get; set; } = 250;

    public int ParallelCameraCount { get; set; } = 1;
}
