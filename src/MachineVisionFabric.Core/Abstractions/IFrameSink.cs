namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Terminal sink for frame data. Receives frames, persists or forwards them,
/// produces no output port values.
/// Lifecycle: WriteAsync (N times) → FlushAsync → DisposeAsync.
/// </summary>
public interface IFrameSink : IAsyncDisposable
{
    Task WriteAsync(IFrameEnvelope frame, CancellationToken cancellationToken);

    /// <summary>
    /// Flushes any buffered frames and finalizes the sink
    /// (e.g., writes session metadata to disk).
    /// </summary>
    Task FlushAsync(CancellationToken cancellationToken);
}
