namespace Mvf.Graph.Execution;

/// <summary>
/// Execution statistics for a single pipeline node collected during one run.
/// </summary>
public sealed class NodeExecutionStats
{
    public required string NodeId { get; init; }

    /// <summary>Total number of cycles in which this node was executed.</summary>
    public required int TotalCycles { get; init; }

    /// <summary>Number of cycles where the node threw an exception (warnings).</summary>
    public required int FaultedCycles { get; init; }

    /// <summary>Total wall-clock milliseconds spent inside ExecuteAsync across all cycles.</summary>
    public required long TotalDurationMs { get; init; }

    /// <summary>Average milliseconds per execution cycle. 0 if no cycles ran.</summary>
    public double AverageDurationMs => TotalCycles > 0 ? (double)TotalDurationMs / TotalCycles : 0;
}
