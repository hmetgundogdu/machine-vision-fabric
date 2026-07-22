namespace Mvf.Graph.Execution;

/// <summary>
/// Summary report produced after a graph execution run completes.
/// </summary>
public sealed class PipelineExecutionReport
{
    public required bool Succeeded { get; init; }

    /// <summary>Total cycles driven by the source node (frames produced).</summary>
    public required int TotalCycles { get; init; }

    /// <summary>Cycles where at least one sink node received output (gate passed).</summary>
    public required int AcceptedCycles { get; init; }

    /// <summary>
    /// Frames dropped under <see cref="BackpressurePolicy.Drop"/> because the shared data plane was full
    /// when a producer tried to publish. Always 0 under <see cref="BackpressurePolicy.Stall"/> (which
    /// fails the run instead) and when the graph has no out-of-process workers.
    /// </summary>
    public int DroppedFrames { get; init; }

    public required TimeSpan Duration { get; init; }

    /// <summary>Human-readable error message when Succeeded is false.</summary>
    public string? ErrorMessage { get; init; }

    public IReadOnlyList<string> Warnings { get; init; } = [];

    /// <summary>Per-node execution statistics. Key is node ID.</summary>
    public IReadOnlyDictionary<string, NodeExecutionStats> NodeStats { get; init; } =
        new Dictionary<string, NodeExecutionStats>();
}
