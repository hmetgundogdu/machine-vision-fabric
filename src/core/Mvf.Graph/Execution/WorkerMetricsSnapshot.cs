namespace Mvf.Graph.Execution;

/// <summary>
/// Cross-process health for one node backed by an out-of-process worker: how much work crossed the
/// process boundary, how long it took, and how often the child had to be recovered. Snapshotted while
/// the run is still alive and carried out in <see cref="NodeExecutionStats"/>, so a crash that the
/// supervisor silently absorbed is still visible after the run (M3 observability).
/// </summary>
public sealed record WorkerMetricsSnapshot
{
    /// <summary>The module id from the worker's <c>hello</c> handshake.</summary>
    public required string ModuleId { get; init; }

    /// <summary>Work requests sent to the worker (frames classified/transformed). Excludes checkpoint RPCs.</summary>
    public long Requests { get; init; }

    /// <summary>Requests that ended in an error — after any restart-and-retry the supervisor performed.</summary>
    public long FailedRequests { get; init; }

    /// <summary>
    /// Total wall-clock microseconds spent inside worker requests, measured from the engine side — so it
    /// includes marshalling, the pipe round-trip, and the child's own compute. Microseconds because a
    /// local RPC is routinely sub-millisecond.
    /// </summary>
    public long TotalRequestMicros { get; init; }

    /// <summary>Slowest single request, in microseconds. A restart shows up here as a latency spike.</summary>
    public long MaxRequestMicros { get; init; }

    /// <summary>Times the child process died and was transparently replaced by the supervisor.</summary>
    public int Restarts { get; init; }

    /// <summary>
    /// Restarts served from a pre-warmed spare (L.4) rather than a cold spawn. <c>Restarts - WarmRestarts</c>
    /// is how often recovery paid the full cold-start.
    /// </summary>
    public int WarmRestarts { get; init; }

    /// <summary>When the most recent restart happened, or null if the worker never died.</summary>
    public DateTime? LastRestartUtc { get; init; }

    /// <summary>The failure that was classified as worker death, for the most recent restart.</summary>
    public string? LastRestartReason { get; init; }

    /// <summary>Average milliseconds per worker request. 0 when no request was made.</summary>
    public double AverageRequestMs => Requests > 0 ? TotalRequestMicros / 1000.0 / Requests : 0;

    /// <summary>Slowest single request in milliseconds.</summary>
    public double MaxRequestMs => MaxRequestMicros / 1000.0;
}
