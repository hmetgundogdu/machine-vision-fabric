namespace MachineVisionFabric.Integrations.SyntheticCamera;

/// <summary>
/// Configuration for the synthetic belt camera. The defaults describe a small, fast scene: big enough to
/// look like an image, small enough that a demo does not move megabytes per frame.
/// </summary>
public sealed class SyntheticCameraOptions
{
    /// <summary>Frame width in pixels.</summary>
    public int Width { get; set; } = 320;

    /// <summary>Frame height in pixels.</summary>
    public int Height { get; set; } = 240;

    /// <summary>Delay between frames in milliseconds (0 = as fast as the pipeline pulls).</summary>
    public int FrameIntervalMs { get; set; } = 200;

    /// <summary>How many frames to emit before the source exhausts. 0 means "keep going".</summary>
    public int FrameCount { get; set; }

    /// <summary>
    /// How many frames one part takes to cross the field of view. Also the scene's period: a part enters,
    /// crosses, leaves, and the next one follows.
    /// </summary>
    public int FramesPerPart { get; set; } = 12;

    /// <summary>
    /// Every Nth part carries a dark defect, so a downstream classifier has something to disagree about.
    /// 0 disables defects entirely.
    /// </summary>
    public int DefectEveryNthPart { get; set; } = 3;

    /// <summary>Fraction of the belt with no part on it at all — the gap between parts (0..1).</summary>
    public double GapRatio { get; set; } = 0.35;

    /// <summary>Seed for the sensor noise, so a run is reproducible.</summary>
    public int Seed { get; set; } = 1;
}
