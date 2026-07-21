namespace Mvf.Abstractions;

/// <summary>
/// Runtime executor for a single pipeline node.
/// Lifecycle: ActivateAsync → ExecuteAsync (N times) → DisposeAsync.
/// </summary>
public interface INodeRunner : IAsyncDisposable
{
    string NodeId { get; }

    /// <summary>
    /// Activates the node (opens connections, starts background threads, pre-loads models).
    /// Called once before the first ExecuteAsync.
    /// </summary>
    Task ActivateAsync(CancellationToken cancellationToken);

    /// <summary>
    /// Executes one cycle of the node given its current input port values.
    /// Returns output port values, or <see cref="NodeExecutionResult.NoOutput"/>
    /// when the node has nothing to emit (stream exhausted, gate closed, etc.).
    /// </summary>
    Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken);
}
