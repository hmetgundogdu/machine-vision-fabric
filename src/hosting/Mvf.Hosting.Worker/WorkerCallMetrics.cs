using System.Diagnostics;
using Mvf.Graph.Execution;

namespace Mvf.Hosting.Worker;

/// <summary>
/// Restart bookkeeping for a worker channel. A raw <see cref="StdioWorkerProcess"/> never restarts, so
/// only <see cref="SupervisedWorker"/> reports anything other than <see cref="None"/>.
/// </summary>
/// <param name="Restarts">Times the child died and was transparently replaced.</param>
/// <param name="WarmRestarts">Of those, how many were served from a pre-warmed spare (L.4).</param>
/// <param name="LastRestartUtc">When the most recent restart happened.</param>
/// <param name="LastRestartReason">The failure that was classified as worker death, most recent first.</param>
public readonly record struct WorkerRestartStats(
    int Restarts,
    int WarmRestarts,
    DateTime? LastRestartUtc,
    string? LastRestartReason)
{
    /// <summary>A channel that cannot restart.</summary>
    public static WorkerRestartStats None => default;
}

/// <summary>
/// Counters for the work RPCs an adapter sends to its worker — request count, failures, and round-trip
/// time measured from the engine side. Held by the adapter (which knows an <c>execute</c> from a
/// checkpoint RPC) rather than the channel, and combined with the channel's
/// <see cref="WorkerRestartStats"/> into the <see cref="WorkerMetricsSnapshot"/> the engine reads.
///
/// <para>Thread-safe: a warm-pool replenish or a supervisor restart can run off the executor's thread.</para>
/// </summary>
internal sealed class WorkerCallMetrics
{
    private long _requests;
    private long _failedRequests;
    private long _totalMicros;
    private long _maxMicros;

    /// <summary>Opens a measurement; pass the returned timestamp to <see cref="Complete"/>.</summary>
    public static long Start() => Stopwatch.GetTimestamp();

    /// <summary>Closes a measurement started at <paramref name="startTimestamp"/>.</summary>
    public void Complete(long startTimestamp, bool failed)
    {
        var micros = (long)Stopwatch.GetElapsedTime(startTimestamp).TotalMicroseconds;

        Interlocked.Increment(ref _requests);
        Interlocked.Add(ref _totalMicros, micros);
        if (failed)
        {
            Interlocked.Increment(ref _failedRequests);
        }

        // Lock-free max — contention here is rare (one adapter, one executor thread).
        var observed = Interlocked.Read(ref _maxMicros);
        while (micros > observed)
        {
            var prior = Interlocked.CompareExchange(ref _maxMicros, micros, observed);
            if (prior == observed)
            {
                break;
            }

            observed = prior;
        }
    }

    /// <summary>Combines these call counters with the channel's identity and restart history.</summary>
    public WorkerMetricsSnapshot Snapshot(IWorkerChannel channel)
    {
        var restarts = channel.RestartStats;
        return new WorkerMetricsSnapshot
        {
            ModuleId = channel.ModuleId,
            Requests = Interlocked.Read(ref _requests),
            FailedRequests = Interlocked.Read(ref _failedRequests),
            TotalRequestMicros = Interlocked.Read(ref _totalMicros),
            MaxRequestMicros = Interlocked.Read(ref _maxMicros),
            Restarts = restarts.Restarts,
            WarmRestarts = restarts.WarmRestarts,
            LastRestartUtc = restarts.LastRestartUtc,
            LastRestartReason = restarts.LastRestartReason
        };
    }
}
