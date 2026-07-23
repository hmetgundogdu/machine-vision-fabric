using Mvf.Abstractions;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Execution;

/// <summary>
/// Picks the executor for a run from <see cref="PipelineExecutionOptions.ExecutionMode"/>. Composition
/// registers this behind <see cref="IPipelineGraphExecutor"/> so the scheduling model is a per-run choice
/// rather than a wiring decision — and so the serial executor stays reachable, unchanged, as the default.
/// </summary>
public sealed class ModeDispatchingGraphExecutor(
    PipelineGraphExecutor serial,
    PipelinedGraphExecutor pipelined) : IPipelineGraphExecutor
{
    public Task<PipelineExecutionReport> ExecuteAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(options);

        return options.ExecutionMode == PipelineExecutionMode.Pipelined
            ? pipelined.ExecuteAsync(definition, options, cancellationToken)
            : serial.ExecuteAsync(definition, options, cancellationToken);
    }
}
