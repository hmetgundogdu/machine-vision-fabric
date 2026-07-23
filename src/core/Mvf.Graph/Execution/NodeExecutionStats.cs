using Mvf.Graph.Runtime;

namespace Mvf.Graph.Execution;

/// <summary>
/// Execution statistics for a single pipeline node collected during one run.
/// </summary>
public sealed class NodeExecutionStats
{
    public required string NodeId { get; init; }

    /// <summary>
    /// Resolved loading profile for this node (node override → module-declared default → resident).
    /// Part of the lifecycle contract made observable (L.1).
    /// </summary>
    public NodeActivationMode ActivationMode { get; init; } = NodeActivationMode.Resident;

    /// <summary>
    /// Wall-clock milliseconds spent activating (warming up) this node before the first cycle — loading a
    /// model, connecting a device, initialising. A slow preload is visible here instead of silent (L.1).
    /// </summary>
    public long WarmupMs { get; init; }

    /// <summary>Total number of cycles in which this node was executed.</summary>
    public required int TotalCycles { get; init; }

    /// <summary>Number of cycles where the node threw an exception (warnings).</summary>
    public required int FaultedCycles { get; init; }

    /// <summary>Total wall-clock milliseconds spent inside ExecuteAsync across all cycles.</summary>
    public required long TotalDurationMs { get; init; }

    /// <summary>
    /// Cross-process counters when this node runs out-of-process (worker RPC latency, restarts), or null
    /// for an in-process node. This is where an absorbed worker crash becomes visible (M3 observability).
    /// </summary>
    public WorkerMetricsSnapshot? Worker { get; init; }

    /// <summary>Average milliseconds per execution cycle. 0 if no cycles ran.</summary>
    public double AverageDurationMs => TotalCycles > 0 ? (double)TotalDurationMs / TotalCycles : 0;
}
