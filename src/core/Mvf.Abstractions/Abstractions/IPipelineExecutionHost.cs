using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Abstractions;

/// <summary>
/// Manages the lifecycle of a long-running pipeline execution.
/// Wraps <see cref="IPipelineGraphExecutor"/> and provides start, stop, and live snapshot capabilities.
///
/// Usage:
/// <code>
/// await host.StartAsync(definition, options);
/// // elsewhere:
/// var snapshot = host.GetSnapshot();
/// await host.StopAsync();
/// var report = await host.WaitForCompletionAsync();
/// </code>
/// </summary>
public interface IPipelineExecutionHost : IAsyncDisposable
{
    /// <summary>
    /// Starts the pipeline in the background.
    /// Throws <see cref="InvalidOperationException"/> if a run is already in progress.
    /// </summary>
    Task StartAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken = default);

    /// <summary>
    /// Requests graceful stop by cancelling the run's CancellationToken.
    /// Returns immediately — use <see cref="WaitForCompletionAsync"/> to await the final report.
    /// </summary>
    Task StopAsync(CancellationToken cancellationToken = default);

    /// <summary>
    /// Returns a point-in-time snapshot of the current execution state.
    /// Thread-safe; can be called at any frequency.
    /// </summary>
    PipelineExecutionSnapshot GetSnapshot();

    /// <summary>
    /// Awaits the completion of the running pipeline and returns the final report.
    /// Returns immediately if no run is in progress (returns the last report or null).
    /// </summary>
    Task<PipelineExecutionReport?> WaitForCompletionAsync(CancellationToken cancellationToken = default);
}
