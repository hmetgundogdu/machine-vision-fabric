namespace Mvf.Graph.Execution;

/// <summary>
/// A point-in-time resource reading for one out-of-process worker: how much memory its child process holds
/// and how hard it is working the CPU. Only a node backed by a real child process can report this — an
/// in-process node shares the host and cannot be attributed a figure of its own, which is why this rides the
/// worker-metrics seam rather than the general node stats.
/// </summary>
public readonly record struct WorkerResourceSample
{
    /// <summary>Resident memory of the child process (Working Set / RSS), in bytes.</summary>
    public long WorkingSetBytes { get; init; }

    /// <summary>Peak resident memory seen so far, in bytes. Best-effort — 0 on platforms that don't track it.</summary>
    public long PeakWorkingSetBytes { get; init; }

    /// <summary>CPU usage since the previous sample, normalised to all cores (0–100). 0 on the first sample
    /// after a (re)start, when there is no prior reading to difference against.</summary>
    public double CpuPercent { get; init; }
}
