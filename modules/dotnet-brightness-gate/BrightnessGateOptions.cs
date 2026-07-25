namespace Mvf.Example.BrightnessGate;

/// <summary>
/// Configuration for the example brightness gate.
/// </summary>
public sealed class BrightnessGateOptions
{
    /// <summary>
    /// Minimum mean byte value (0–255). Frames whose average byte falls below this
    /// are rejected as "too dark". Default: 18.0.
    /// </summary>
    public double MinimumMeanByte { get; set; } = 18.0;

    /// <summary>Log accept/reject decisions with the measured mean.</summary>
    public bool LogDecisions { get; set; } = false;
}
