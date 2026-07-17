using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution.NodeRunners;

/// <summary>
/// Drives an <see cref="IFrameSourceSession"/> as a source node.
/// Each ExecuteAsync call advances the async enumerator by one frame.
/// Returns NoOutput when the source stream is exhausted.
/// </summary>
internal sealed class FrameSourceNodeRunner(string nodeId, IFrameSourceSession session) : INodeRunner
{
    private IAsyncEnumerator<IFrameEnvelope>? _enumerator;
    private bool _exhausted;

    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken)
    {
        _enumerator = session.ReadFramesAsync(cancellationToken).GetAsyncEnumerator(cancellationToken);
        return Task.CompletedTask;
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
