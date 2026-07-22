using Mvf.Abstractions;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Wraps an <see cref="IFrameTransformer"/> as a compute node that emits a <b>new</b> frame.
/// Reads a frame, transforms it, and emits the result — or nothing when the transformer drops it.
/// This is how an out-of-process (e.g. Python) module produces frame data back into a .NET graph.
///
/// Input ports : <c>frame</c> (data)
/// Output ports: <c>frame</c> (data) — emitted only when the transformer returns a frame
/// </summary>
internal sealed class FrameTransformerNodeRunner(string nodeId, IFrameTransformer transformer) : INodeRunner
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

        var output = await transformer.TransformAsync(frameInput.Frame, cancellationToken);
        return output is null
            ? NodeExecutionResult.NoOutput
            : NodeExecutionResult.Single("frame", PortValue.FromFrame(output));
    }

    // The transformer may own an out-of-process worker; dispose it when disposable.
    public async ValueTask DisposeAsync()
    {
        switch (transformer)
        {
            case IAsyncDisposable asyncDisposable:
                await asyncDisposable.DisposeAsync();
                break;
            case IDisposable disposable:
                disposable.Dispose();
                break;
        }
    }
}
