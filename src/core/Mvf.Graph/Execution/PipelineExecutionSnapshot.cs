namespace Mvf.Graph.Execution;

/// <summary>
/// Immutable real-time snapshot of a running (or finished) pipeline execution.
/// Obtain via <c>IPipelineExecutionHost.GetSnapshot()</c>.
/// </summary>
public sealed class PipelineExecutionSnapshot
{
    public required PipelineExecutionStatus Status { get; init; }

    /// <summary>Unique run identifier. Null when Idle.</summary>
    public required string? RunId { get; init; }

    /// <summary>Pipeline name from the definition. Null when Idle.</summary>
    public required string? PipelineName { get; init; }

    /// <summary>Total completed cycles so far.</summary>
    public required int TotalCycles { get; init; }

    /// <summary>Cycles where at least one sink received a frame.</summary>
    public required int AcceptedCycles { get; init; }

    /// <summary>Wall-clock time elapsed since the run started.</summary>
    public required TimeSpan Elapsed { get; init; }

    /// <summary>UTC time the current run started. Null when Idle.</summary>
    public required DateTime? StartedAt { get; init; }

    /// <summary>Last error message when Status is Faulted.</summary>
    public required string? LastError { get; init; }
}
