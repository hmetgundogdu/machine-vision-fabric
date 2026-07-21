namespace MachineVisionFabric.Integrations.BlackScreenCheck;

/// <summary>
/// Configuration for the OpenCV-based black-screen check.
///
/// This is the inverse of a dark-frame filter: it PASSES frames whose mean
/// grayscale brightness is at or below <see cref="DarknessThreshold"/> (i.e. the
/// "very black" frames), so a downstream branch can persist/flag them. Brighter
/// frames are dropped from this branch.
/// </summary>
public sealed class BlackScreenCheckOptions
{
    /// <summary>
    /// Mean grayscale brightness (0–255) at or below which a frame is considered
    /// "very black". Frames darker than this are passed through this branch.
    /// Default: 40.0 — tune for your lighting.
    /// </summary>
    public double DarknessThreshold { get; set; } = 40.0;

    /// <summary>
    /// When a frame cannot be decoded by OpenCV, treat it as black (pass it on).
    /// A frame that fails to decode is usually a bad capture worth inspecting.
    /// </summary>
    public bool TreatDecodeFailureAsBlack { get; set; } = true;

    /// <summary>Emit a warning line for every black frame detected.</summary>
    public bool LogWarnings { get; set; } = true;
}
