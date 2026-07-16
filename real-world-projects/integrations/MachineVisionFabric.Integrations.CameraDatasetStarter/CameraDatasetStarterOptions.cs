namespace MachineVisionFabric.Integrations.CameraDatasetStarter;

public sealed class CameraDatasetStarterOptions
{
    public string SourceFolder { get; set; } = ".\\assets\\images";

    public string CameraId { get; set; } = "project-cam-1";

    public bool Loop { get; set; } = true;

    public int FrameIntervalMs { get; set; } = 100;

    public int BoundedCapacity { get; set; } = 8;

    public int? MaxFrames { get; set; }

    public bool DeliverMemoryFrames { get; set; } = true;
}
