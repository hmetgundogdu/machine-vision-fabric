namespace Mvf.Graph.Execution;

/// <summary>
/// A typed control-channel value emitted by gate, classifier and control nodes.
/// The control channel deliberately carries decisions and scalar measurements —
/// never image data. Frames travel only on data edges.
/// </summary>
public sealed class ControlSignal
{
    /// <summary>
    /// Identifies the signal contract.
    /// Well-known values: <c>boolean-gate</c>, <c>classification</c>, <c>measurement</c>.
    /// </summary>
    public required string SignalType { get; init; }

    /// <summary>
    /// The boolean gate decision (true = product present / condition met).
    /// Used when SignalType is <c>boolean-gate</c>.
    /// </summary>
    public required bool Value { get; init; }

    /// <summary>
    /// Optional string classification label used by the <c>switch</c> primitive for routing.
    /// Used when SignalType is <c>classification</c>.
    /// Example: "accept", "reject", "quarantine".
    /// </summary>
    public string? ClassLabel { get; init; }

    /// <summary>
    /// Optional scalar measurement carried on the control channel, e.g. a mean brightness,
    /// an object count, or a measured dimension derived from a frame. Used when SignalType is
    /// <c>measurement</c>, and may also accompany a <c>classification</c> for downstream logic.
    /// </summary>
    public double? Measurement { get; init; }

    /// <summary>Unit for <see cref="Measurement"/> (e.g. "px", "mm", "count"). Optional.</summary>
    public string? Unit { get; init; }

    public string Source { get; init; } = string.Empty;

    public string StationId { get; init; } = string.Empty;

    public DateTime TimestampUtc { get; init; } = DateTime.UtcNow;

    public string? Details { get; init; }
}
