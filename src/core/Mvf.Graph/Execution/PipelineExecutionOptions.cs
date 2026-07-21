namespace Mvf.Graph.Execution;

/// <summary>
/// Runtime options passed into the graph executor for a single execution run.
/// </summary>
public sealed class PipelineExecutionOptions
{
    /// <summary>
    /// Absolute path to the package directory.
    /// Used by integration modules to resolve relative asset paths.
    /// </summary>
    public required string PackageRoot { get; init; }

    /// <summary>
    /// Absolute path to the integration module plugin directory.
    /// </summary>
    public required string IntegrationsRoot { get; init; }

    /// <summary>
    /// Maximum number of source cycles to execute before stopping.
    /// 0 means run until the source is exhausted or cancelled.
    /// </summary>
    public int MaxCycles { get; init; } = 0;

    /// <summary>
    /// Optional callback invoked at the end of each completed cycle.
    /// Use to observe real-time progress without polling.
    /// </summary>
    public Action<PipelineExecutionProgress>? OnCycleCompleted { get; init; }

    /// <summary>
    /// Optional callback invoked immediately after each node finishes executing.
    /// Fired before cycle-level routing — allows per-node observability.
    /// </summary>
    public Action<NodeExecutionEvent>? OnNodeExecuted { get; init; }
}
