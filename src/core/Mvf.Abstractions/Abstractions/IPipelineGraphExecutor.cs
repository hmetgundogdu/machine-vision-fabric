using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Abstractions;

/// <summary>
/// Top-level graph executor. Resolves node runners, runs topological cycles,
/// routes port values via edges, and returns a summary report.
/// </summary>
public interface IPipelineGraphExecutor
{
    Task<PipelineExecutionReport> ExecuteAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken);
}
