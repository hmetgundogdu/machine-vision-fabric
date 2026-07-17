namespace MachineVisionFabric.Integrations.OpenCvDarkFrameGate;

public sealed class OpenCvDarkFrameGateOptions
{
    public double MinimumMeanBrightness { get; set; } = 18.0;

    public string SourceName { get; set; } = "opencv-dark-frame-gate";

    public string StrategyName { get; set; } = "mean-grayscale-threshold";

    public bool RejectOnDecodeFailure { get; set; } = true;

    public bool LogDecisions { get; set; } = true;
}
