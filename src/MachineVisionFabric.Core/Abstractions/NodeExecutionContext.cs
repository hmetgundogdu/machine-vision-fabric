namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Contextual metadata injected into every node on each execution cycle.
/// Nodes may use this for logging, rate control, or diagnostics.
/// </summary>
public sealed class NodeExecutionContext
{
    /// <summary>Unique identifier for this execution run (stable across all cycles of one run).</summary>
    public required string RunId { get; init; }

    /// <summary>Zero-based cycle index within the current run.</summary>
    public required int CycleIndex { get; init; }

    /// <summary>Wall-clock UTC time when this cycle started.</summary>
    public required DateTime CycleStartedAt { get; init; }
}
