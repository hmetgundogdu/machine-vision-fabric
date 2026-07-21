namespace Mvf.Graph.Execution;

/// <summary>
/// Progress snapshot emitted at the end of each completed execution cycle.
/// Delivered via <see cref="PipelineExecutionOptions.OnCycleCompleted"/>.
/// </summary>
public sealed class PipelineExecutionProgress
{
    /// <summary>Unique identifier for the current run.</summary>
    public required string RunId { get; init; }

    /// <summary>Zero-based cycle index of the cycle that just completed.</summary>
    public required int CycleIndex { get; init; }

    /// <summary>Total completed cycles so far (equals CycleIndex + 1).</summary>
    public required int TotalCycles { get; init; }

    /// <summary>Accepted cycles (gate passed) so far.</summary>
    public required int AcceptedCycles { get; init; }

    /// <summary>Whether the cycle that just completed had its frame accepted by the sink.</summary>
    public required bool CycleAccepted { get; init; }

    /// <summary>Elapsed time since the run started.</summary>
    public required TimeSpan Elapsed { get; init; }
}
