namespace Mvf.Graph.Execution;

/// <summary>
/// Where a <b>source</b> node's per-frame time went, aggregated over a run — the source counterpart to
/// <see cref="StageProfile"/>. Non-source nodes carry no <see cref="SourceProfile"/>. Unlike a stage
/// profile these gauges do not partition one span: receive runs on the producer thread and overlaps the
/// wait (see <see cref="FrameAcquisitionSample"/>). Averages are over frames that reported the gauge, so
/// <see cref="AverageReceiveMs"/> stays meaningful even before any source opts into receive timing.
/// </summary>
public sealed record SourceProfile
{
    /// <summary>Frames whose acquisition this run measured (i.e. that reported a wait/queue sample).</summary>
    public long Frames { get; init; }

    /// <summary>Frames that additionally reported a receive time (the source called <c>BeginAcquire()</c>).</summary>
    public long ReceiveFrames { get; init; }

    /// <summary>Most recent frame's receive/queue/wait, in microseconds (receive 0 when unmeasured).</summary>
    public long LastReceiveMicros { get; init; }
    public long LastQueueMicros { get; init; }
    public long LastWaitMicros { get; init; }

    /// <summary>Running totals for the averages below, in microseconds.</summary>
    public long TotalReceiveMicros { get; init; }
    public long TotalQueueMicros { get; init; }
    public long TotalWaitMicros { get; init; }

    /// <summary>True once at least one frame reported a receive time, so the UI can show "recv" vs "-".</summary>
    public bool HasReceive => ReceiveFrames > 0;

    public double LastReceiveMs => LastReceiveMicros / 1000.0;
    public double LastQueueMs => LastQueueMicros / 1000.0;
    public double LastWaitMs => LastWaitMicros / 1000.0;

    public double AverageReceiveMs => ReceiveFrames > 0 ? TotalReceiveMicros / 1000.0 / ReceiveFrames : 0;
    public double AverageQueueMs => Frames > 0 ? TotalQueueMicros / 1000.0 / Frames : 0;
    public double AverageWaitMs => Frames > 0 ? TotalWaitMicros / 1000.0 / Frames : 0;
}
