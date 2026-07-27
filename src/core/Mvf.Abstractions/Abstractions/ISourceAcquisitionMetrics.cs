using Mvf.Graph.Execution;

namespace Mvf.Abstractions;

/// <summary>
/// Implemented by a source (or a runner fronting one) so the engine can read how long the <i>last frame</i>
/// took to acquire — the time the image spent being pulled off the device and sitting in the source's queue,
/// which the node's own <c>ExecuteAsync</c> span cannot show because that work happens on the producer
/// thread. Polled after <c>ExecuteAsync</c> exactly as <see cref="IWorkerMetricsSource"/> is, so the engine
/// observes acquisition without depending on cameras, HTTP, or channels. A node runner forwards for whatever
/// it wraps, the same way it already does for <see cref="IWorkerMetricsSource"/> and <see cref="ICheckpointable"/>.
/// </summary>
public interface ISourceAcquisitionMetrics
{
    /// <summary>
    /// The timing of the most recently produced frame, or null when none has been produced yet. Meant to be
    /// read right after a source's <c>ExecuteAsync</c> returns a frame, so the sample matches that frame.
    /// </summary>
    FrameAcquisitionSample? GetLastAcquisition();
}
