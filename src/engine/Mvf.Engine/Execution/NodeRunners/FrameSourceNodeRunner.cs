using Mvf.Graph.Execution;
using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// A source whose stream can be restarted from the beginning. This is what lets the <c>loop</c>'s
/// <c>forever</c> policy replay a finite source (a folder) — the loop owns "keep going", and it rewinds the
/// source rather than the source carrying its own replay flag. A source that cannot rewind simply does not
/// implement this, and <c>forever</c> then behaves as <c>until-exhausted</c> for it.
/// </summary>
internal interface IRewindableSource
{
    Task RewindAsync(CancellationToken cancellationToken);
}

/// <summary>
/// Drives an <see cref="IFrameSourceSession"/> as a source node.
/// Each ExecuteAsync call advances the async enumerator by one frame.
/// Returns NoOutput when the source stream is exhausted.
/// </summary>
internal sealed class FrameSourceNodeRunner(string nodeId, IFrameSourceSession session)
    : INodeRunner, IRewindableSource, ISourceAcquisitionMetrics
{
    private IAsyncEnumerator<IFrameEnvelope>? _enumerator;
    private bool _exhausted;

    public string NodeId { get; } = nodeId;

    /// <summary>Forwards the wrapped session's per-frame acquisition timing, exactly as this runner forwards
    /// worker metrics — null when the session does not track it (not a background source).</summary>
    public FrameAcquisitionSample? GetLastAcquisition() =>
        (session as ISourceAcquisitionMetrics)?.GetLastAcquisition();

    public Task ActivateAsync(CancellationToken cancellationToken)
    {
        _enumerator = session.ReadFramesAsync(cancellationToken).GetAsyncEnumerator(cancellationToken);
        return Task.CompletedTask;
    }

    /// <summary>
    /// Restarts the stream from the beginning: dispose the spent enumerator and open a fresh one. The
    /// session's <c>ReadFramesAsync</c> yields a new enumeration from the start, so a folder replays from
    /// its first frame.
    /// </summary>
    public async Task RewindAsync(CancellationToken cancellationToken)
    {
        if (_enumerator is not null)
        {
            await _enumerator.DisposeAsync();
        }

        _enumerator = session.ReadFramesAsync(cancellationToken).GetAsyncEnumerator(cancellationToken);
        _exhausted = false;
    }

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        if (_exhausted || _enumerator is null)
        {
            return NodeExecutionResult.NoOutput;
        }

        if (!await _enumerator.MoveNextAsync())
        {
            _exhausted = true;
            return NodeExecutionResult.NoOutput;
        }

        return NodeExecutionResult.Single("frame", PortValue.FromFrame(_enumerator.Current));
    }

    public async ValueTask DisposeAsync()
    {
        if (_enumerator is not null)
        {
            await _enumerator.DisposeAsync();
        }

        await session.DisposeAsync();
    }
}
