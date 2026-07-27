namespace Mvf.Graph.Execution;

/// <summary>
/// Timing for one frame arriving from a source, split into gauges that a single node span cannot show.
/// A background source decouples producing from consuming (fetch on a producer thread, dequeue in the
/// node's <c>ExecuteAsync</c>), so the node's wall-clock is almost entirely <see cref="WaitMicros"/> — it
/// hides how long the image actually took to pull off the camera. These three are <b>independent
/// measurements, not slices of one span</b>: the receive happens on the producer thread and overlaps the
/// wait, so they do not sum to the node duration.
/// </summary>
public sealed record FrameAcquisitionSample
{
    /// <summary>
    /// Microseconds spent actually acquiring the image once one was available — the HTTP fetch / device
    /// read the producer did before publishing. Null when the source did not measure it (a source only
    /// gets this number by wrapping its fetch in <c>BeginAcquire()</c>); wait and queue still populate.
    /// </summary>
    public long? AcquireMicros { get; init; }

    /// <summary>Microseconds this frame sat in the source's queue between being published and being dequeued
    /// by the node. High here means the pipeline is falling behind the camera, not that the camera is slow.</summary>
    public long QueueMicros { get; init; }

    /// <summary>Microseconds between the previous frame's dequeue and this one's — the inter-frame wait,
    /// dominated by the machine/trigger cadence. This is essentially what the node's own span already showed.</summary>
    public long WaitMicros { get; init; }

    /// <summary>Microseconds from the frame's own capture timestamp to when the source queued it — the
    /// glass-to-fabric lag. Null when the frame carries no reliable capture time.</summary>
    public long? FreshnessMicros { get; init; }
}
