using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution;

/// <summary>
/// Pull-based synchronous cycle executor for typed pipeline graphs.
///
/// Execution model per cycle:
/// <list type="number">
///   <item>Nodes are ordered topologically (Kahn's algorithm, stable order).</item>
///   <item>Each node is executed in order; its output values are routed via edges to downstream input ports.</item>
///   <item>Source nodes drive the loop — when a source returns NoOutput the run ends.</item>
///   <item>Control and data edges use the same routing mechanism but are kept semantically distinct by the port bus.</item>
/// </list>
/// </summary>
public sealed class PipelineGraphExecutor(IPipelineNodeActivator nodeActivator) : IPipelineGraphExecutor
{
    public async Task<PipelineExecutionReport> ExecuteAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(definition);
        ArgumentNullException.ThrowIfNull(options);

        var runId = Guid.NewGuid().ToString("N");
        var startedAt = DateTime.UtcNow;
        var warnings = new List<string>();

        IReadOnlyList<PipelineNodeDefinition> executionOrder;
        try
        {
            executionOrder = GraphTopologySorter.Sort(definition);
        }
        catch (InvalidOperationException ex)
        {
            return Failure(ex.Message, startedAt);
        }

        // Activate all nodes
        var runners = new List<INodeRunner>(executionOrder.Count);
        try
        {
            foreach (var node in executionOrder)
            {
                var runner = await nodeActivator.ActivateAsync(node, options, cancellationToken);
                runners.Add(runner);
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            await DisposeAllAsync(runners);
            return Failure($"Node activation failed: {ex.Message}", startedAt);
        }

        // Build a nodeId → runner lookup
        var runnerById = runners.ToDictionary(
            r => r.NodeId,
            r => r,
            StringComparer.OrdinalIgnoreCase);

        // Per-node mutable stats accumulators
        var statsMap = executionOrder.ToDictionary(
            n => n.Id,
            _ => new NodeStatsAccumulator(),
            StringComparer.OrdinalIgnoreCase);

        var portBus = new GraphPortBus();
        var totalCycles = 0;
        var acceptedCycles = 0;

        try
        {
            while (true)
            {
                cancellationToken.ThrowIfCancellationRequested();

                if (options.MaxCycles > 0 && totalCycles >= options.MaxCycles)
                {
                    break;
                }

                var cycleStartedAt = DateTime.UtcNow;
                var context = new NodeExecutionContext
                {
                    RunId = runId,
                    CycleIndex = totalCycles,
                    CycleStartedAt = cycleStartedAt
                };

                portBus.ClearCycle();
                var sourcesExhausted = false;
                var cycleHadSinkOutput = false;

                foreach (var node in executionOrder)
                {
                    if (!runnerById.TryGetValue(node.Id, out var runner))
                    {
                        continue;
                    }

                    var inputs = portBus.CollectInputs(node.Id, node.Inputs, context);
                    NodeExecutionResult result;
                    var nodeStart = DateTime.UtcNow;
                    var faulted = false;

                    try
                    {
                        result = await runner.ExecuteAsync(inputs, cancellationToken);
                    }
                    catch (Exception ex) when (ex is not OperationCanceledException)
                    {
                        warnings.Add($"Node '{node.Id}' threw during execution: {ex.Message}");
                        result = NodeExecutionResult.NoOutput;
                        faulted = true;
                    }

                    var nodeElapsed = (long)(DateTime.UtcNow - nodeStart).TotalMilliseconds;
                    if (statsMap.TryGetValue(node.Id, out var acc))
                    {
                        acc.TotalCycles++;
                        acc.TotalDurationMs += nodeElapsed;
                        if (faulted) acc.FaultedCycles++;
                    }

                    options.OnNodeExecuted?.Invoke(new NodeExecutionEvent
                    {
                        RunId = runId,
                        NodeId = node.Id,
                        CycleIndex = totalCycles,
                        HasOutput = result.HasOutput,
                        Faulted = faulted,
                        DurationMs = nodeElapsed,
                        OutputPortNames = result.HasOutput
                            ? result.All.Select(kvp => kvp.Key).ToList()
                            : [],
                        InputPortNames = inputs.All.Select(kvp => kvp.Key).ToList()
                    });

                    if (IsSourceNode(node) && !result.HasOutput)
                    {
                        sourcesExhausted = true;
                        break;
                    }

                    if (IsSinkNode(node) && inputs.Has("frame"))
                    {
                        cycleHadSinkOutput = true;
                    }

                    portBus.RouteOutputs(node.Id, result, definition.Edges);
                }

                if (sourcesExhausted)
                {
                    break;
                }

                totalCycles++;
                if (cycleHadSinkOutput)
                {
                    acceptedCycles++;
                }

                options.OnCycleCompleted?.Invoke(new PipelineExecutionProgress
                {
                    RunId = runId,
                    CycleIndex = totalCycles - 1,
                    TotalCycles = totalCycles,
                    AcceptedCycles = acceptedCycles,
                    CycleAccepted = cycleHadSinkOutput,
                    Elapsed = DateTime.UtcNow - startedAt
                });
            }
        }
        catch (OperationCanceledException)
        {
            // Propagate after cleanup
        }
        finally
        {
            await DisposeAllAsync(runners);
        }

        cancellationToken.ThrowIfCancellationRequested();

        var nodeStats = statsMap.ToDictionary(
            kvp => kvp.Key,
            kvp => new NodeExecutionStats
            {
                NodeId = kvp.Key,
                TotalCycles = kvp.Value.TotalCycles,
                FaultedCycles = kvp.Value.FaultedCycles,
                TotalDurationMs = kvp.Value.TotalDurationMs
            },
            StringComparer.OrdinalIgnoreCase);

        return new PipelineExecutionReport
        {
            Succeeded = true,
            TotalCycles = totalCycles,
            AcceptedCycles = acceptedCycles,
            Duration = DateTime.UtcNow - startedAt,
            Warnings = warnings,
            NodeStats = nodeStats
        };
    }

    private static bool IsSourceNode(PipelineNodeDefinition node) =>
        string.Equals(node.Category, "source", StringComparison.OrdinalIgnoreCase)
        || (string.Equals(node.Kind, "runtime-builtin", StringComparison.OrdinalIgnoreCase)
            && string.Equals(node.BuiltinType, "folder-sequence-source", StringComparison.OrdinalIgnoreCase));

    private static bool IsSinkNode(PipelineNodeDefinition node) =>
        string.Equals(node.Category, "output", StringComparison.OrdinalIgnoreCase)
        || string.Equals(node.Category, "sink", StringComparison.OrdinalIgnoreCase);

    private static PipelineExecutionReport Failure(string message, DateTime startedAt) =>
        new()
        {
            Succeeded = false,
            TotalCycles = 0,
            AcceptedCycles = 0,
            Duration = DateTime.UtcNow - startedAt,
            ErrorMessage = message
        };

    private static async Task DisposeAllAsync(IEnumerable<INodeRunner> runners)
    {
        foreach (var runner in runners)
        {
            try
            {
                await runner.DisposeAsync();
            }
            catch
            {
                // best effort — do not mask the original exception
            }
        }
    }

    private sealed class NodeStatsAccumulator
    {
        public int TotalCycles;
        public int FaultedCycles;
        public long TotalDurationMs;
    }
}
