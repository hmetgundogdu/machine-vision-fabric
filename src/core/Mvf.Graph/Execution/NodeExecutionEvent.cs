namespace Mvf.Graph.Execution;

/// <summary>
/// Fired after a single node completes one execution cycle.
/// Delivered via <see cref="PipelineExecutionOptions.OnNodeExecuted"/>.
/// </summary>
public sealed class NodeExecutionEvent
{
    public required string RunId { get; init; }
    public required string NodeId { get; init; }
    public required int CycleIndex { get; init; }
    public required bool HasOutput { get; init; }
    public required bool Faulted { get; init; }
    /// <summary>Wall-clock microseconds this node spent in ExecuteAsync — see NodeExecutionStats.</summary>
    public required long DurationMicros { get; init; }

    /// <summary>The same duration in milliseconds, rounded.</summary>
    public long DurationMs => (long)Math.Round(DurationMicros / 1000.0);

    /// <summary>Port names that carried values out of the node this cycle.</summary>
    public required IReadOnlyList<string> OutputPortNames { get; init; }

    /// <summary>Port names that had values arriving into the node this cycle.</summary>
    public required IReadOnlyList<string> InputPortNames { get; init; }

    /// <summary>
    /// Restarts this node's out-of-process worker has needed so far in the run (0 for an in-process node).
    /// Cumulative, so a consumer sees the count go up on the cycle the crash was absorbed — which is what
    /// makes recovery visible live rather than only in the final report.
    /// </summary>
    public int WorkerRestarts { get; init; }

    /// <summary>
    /// This cycle's frame-acquisition timing when the node is a source, or null for a non-source node (and
    /// for a cycle that produced no frame). Lets the live TUI show receive/wait/queue as the frame lands,
    /// not just in the final report.
    /// </summary>
    public FrameAcquisitionSample? Acquisition { get; init; }
}
