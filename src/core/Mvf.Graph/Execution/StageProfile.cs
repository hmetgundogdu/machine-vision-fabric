namespace Mvf.Graph.Execution;

/// <summary>
/// Where one pipelined stage's wall-clock time went. The four buckets are disjoint and together account
/// for the stage's lifetime, which is what makes them diagnostic: if throughput did not improve, exactly
/// one of them explains it.
/// <list type="bullet">
///   <item><see cref="BusyMicros"/> — inside the node's own ExecuteAsync (the useful work).</item>
///   <item><see cref="RouteMicros"/> — publishing into the arena and handing values to edges, minus any
///   time spent blocked on a full queue. Serial mode attributes this to nobody, which is why per-node
///   time never summed to wall clock.</item>
///   <item><see cref="WriteBlockedMicros"/> — parked on a full downstream queue. High here means the
///   stage is faster than its consumer: backpressure is working and the consumer is the bottleneck.</item>
///   <item><see cref="ReadBlockedMicros"/> — waiting for an input value. High here means the stage is
///   starved by its producer.</item>
/// </list>
/// A stage where all four are small relative to the run duration is not being scheduled at all.
/// </summary>
public sealed record StageProfile
{
    /// <summary>Time inside the node's ExecuteAsync.</summary>
    public long BusyMicros { get; init; }

    /// <summary>Time spent publishing and delivering outputs, excluding blocking on a full queue.</summary>
    public long RouteMicros { get; init; }

    /// <summary>Time parked because a downstream queue was full.</summary>
    public long WriteBlockedMicros { get; init; }

    /// <summary>Time parked waiting for an input value.</summary>
    public long ReadBlockedMicros { get; init; }

    public double BusyMs => BusyMicros / 1000.0;
    public double RouteMs => RouteMicros / 1000.0;
    public double WriteBlockedMs => WriteBlockedMicros / 1000.0;
    public double ReadBlockedMs => ReadBlockedMicros / 1000.0;

    /// <summary>Everything accounted for, in milliseconds.</summary>
    public double TotalMs => (BusyMicros + RouteMicros + WriteBlockedMicros + ReadBlockedMicros) / 1000.0;
}
