namespace MachineVisionFabric.Contracts.Execution;

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

    public required TimeSpan Duration { get; init; }

    /// <summary>Human-readable error message when Succeeded is false.</summary>
    public string? ErrorMessage { get; init; }

    public IReadOnlyList<string> Warnings { get; init; } = [];

    /// <summary>Per-node execution statistics. Key is node ID.</summary>
    public IReadOnlyDictionary<string, NodeExecutionStats> NodeStats { get; init; } =
        new Dictionary<string, NodeExecutionStats>();
}
