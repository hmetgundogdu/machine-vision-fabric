namespace Mvf.Graph.Execution;

/// <summary>
/// The realtime-egress seam: a running pipeline hands each cycle / node-transition to a sink so an
/// external panel / studio / observer can watch it live.
///
/// <para><b>Off the hot path, always.</b> Both methods must be non-blocking and best-effort — an
/// implementation enqueues into a bounded ring and returns immediately, dropping (and counting) rather
/// than ever stalling the executor. This is <i>publish</i>, not a data edge: it never participates in
/// refcounts or ordering. The engine calls these beside the existing observation callbacks; the actual
/// socket lives behind this interface in a separate project the core never references.</para>
/// </summary>
public interface IEgressSink : IDisposable
{
    /// <summary>Publish end-of-cycle state. Non-blocking; drops when the ring is full.</summary>
    void PublishCycle(PipelineExecutionProgress progress);

    /// <summary>Publish a node-transition (state only, no payload). Non-blocking; drops when the ring is
    /// full.</summary>
    void PublishNodeTransition(NodeExecutionEvent nodeEvent);

    /// <summary>
    /// True when the sink actually wants frame payloads right now — i.e. it is configured to stream frames
    /// <b>and</b> a subscriber is attached. The engine checks this <b>before</b> doing the bounded work of
    /// reading a frame's bytes, so an unwatched run pays nothing for frame-data egress.
    /// </summary>
    bool WantsFrames { get; }

    /// <summary>
    /// Publish a node-transition carrying frame-data. <paramref name="payloadDescriptor"/> is the 192-byte
    /// <c>PayloadDescriptor</c> header; <paramref name="payload"/> is the raw frame bytes. The caller hands
    /// over ownership of both buffers. Non-blocking; drops when the ring is full.
    /// </summary>
    void PublishNodeTransitionFrame(NodeExecutionEvent nodeEvent, ReadOnlyMemory<byte> payloadDescriptor, ReadOnlyMemory<byte> payload);

    /// <summary>A live count of what the sink has published, dropped, and how many subscribers are attached.</summary>
    EgressSinkStats Stats { get; }
}

/// <summary>Best-effort counters a sink exposes for the TUI/report — published vs dropped tells the
/// operator whether a slow or absent consumer is causing loss (expected, not an error).</summary>
public readonly record struct EgressSinkStats(long Published, long Dropped, int Subscribers);
