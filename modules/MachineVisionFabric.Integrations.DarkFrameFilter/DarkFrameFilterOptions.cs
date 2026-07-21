namespace MachineVisionFabric.Integrations.DarkFrameFilter;

/// <summary>
/// Configuration for the OpenCV-based dark-frame filter.
/// </summary>
public sealed class DarkFrameFilterOptions
{
    /// <summary>
    /// Minimum mean grayscale brightness (0–255).
    /// Frames below this value are rejected as "dark".
    /// Default: 18.0 — adjust for your lighting conditions.
    /// </summary>
    public double MinimumMeanBrightness { get; set; } = 18.0;

    /// <summary>Reject frames that cannot be decoded by OpenCV.</summary>
    public bool RejectOnDecodeFailure { get; set; } = true;

    /// <summary>Log accept/reject decisions with brightness values.</summary>
    public bool LogDecisions { get; set; } = true;
}
