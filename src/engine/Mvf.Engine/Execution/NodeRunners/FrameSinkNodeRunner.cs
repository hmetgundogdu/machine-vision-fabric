using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Terminal sink node runner. Forwards every accepted frame to an <see cref="IFrameSink"/>.
/// Returns <see cref="NodeExecutionResult.NoOutput"/> always — sinks are pipeline terminals.
///
/// Input ports : <c>frame</c> (data)
/// Output ports: (none)
/// </summary>
internal sealed class FrameSinkNodeRunner(string nodeId, IFrameSink sink) : INodeRunner
{
    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var frameInput = inputs.Get("frame");
        if (frameInput?.Frame is null)
        {
            return NodeExecutionResult.NoOutput;
        }

        await sink.WriteAsync(frameInput.Frame, cancellationToken);
        return NodeExecutionResult.NoOutput;
    }

    public async ValueTask DisposeAsync()
    {
        await sink.FlushAsync(CancellationToken.None);
        await sink.DisposeAsync();
    }
}
