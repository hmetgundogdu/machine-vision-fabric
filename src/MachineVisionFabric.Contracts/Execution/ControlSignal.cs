namespace MachineVisionFabric.Contracts.Execution;

/// <summary>
/// A typed control-channel value emitted by gate and control nodes.
/// </summary>
public sealed class ControlSignal
{
    /// <summary>
    /// Identifies the signal contract.
    /// Well-known values: <c>boolean-gate</c>, <c>classification</c>.
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

    public string Source { get; init; } = string.Empty;

    public string StationId { get; init; } = string.Empty;

    public DateTime TimestampUtc { get; init; } = DateTime.UtcNow;

    public string? Details { get; init; }
}
