using System.Diagnostics;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Modules;
using Mvf.Engine.Recovery;
using Mvf.Graph.Runtime;

namespace Mvf.Engine.Execution;

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
///
/// <para><b>Graph-aware data plane (M2):</b> when an <see cref="IDataPlane"/> is present, a frame that
/// fans out to one or more out-of-process (worker) consumers is published into the shared arena
/// <b>once</b> — with a reference count equal to the number of worker edges — and the resulting
/// <see cref="ArenaFrameEnvelope"/> is routed to those workers (in-process consumers keep the heap
/// frame, zero copy). Each consumer's handle is released after it runs, so the slot is reclaimed once
/// the last worker has read it. Transport is thus chosen per edge from the static graph.</para>
/// </summary>
public sealed class PipelineGraphExecutor(
    IPipelineNodeActivator nodeActivator,
    IDataPlane? dataPlane = null,
    ModuleCatalog? moduleCatalog = null) : IPipelineGraphExecutor
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

        // Module catalog (metadata only, no DLL load) resolves each node's declared lifecycle + which
        // nodes run out-of-process. Loaded once and reused.
        var loadedCatalog = moduleCatalog?.Load(options.IntegrationsRoot);

        // Lifecycle contract made observable (L.1): time each node's activation (warmup — model load,
        // device connect, init) and record its resolved loading profile. No behavior change yet.
        var warmupByNode = new Dictionary<string, long>(StringComparer.OrdinalIgnoreCase);
        var activationModeByNode = new Dictionary<string, NodeActivationMode>(StringComparer.OrdinalIgnoreCase);

        // Per-producing-node backpressure policy (node override → module default → run default), so a
        // source can pick lossless vs lossy (folder-replay stalls, live camera drops).
        var backpressureByNode = new Dictionary<string, BackpressurePolicy>(StringComparer.OrdinalIgnoreCase);

        // Resolve every node's loading profile first (cheap — no activation). Resident nodes are
        // preloaded before cycle 0; on-demand nodes are activated lazily on first use (L.3).
        foreach (var node in executionOrder)
        {
            activationModeByNode[node.Id] = ResolveActivationMode(node, loadedCatalog);
            backpressureByNode[node.Id] = ResolveBackpressurePolicy(node, loadedCatalog, options.BackpressurePolicy);
        }

        // Durable checkpoints (engine-crash resume) when a directory is configured; otherwise captures
        // stay in memory (worker-crash recovery only). Restore states are loaded up front so a node can
        // be restored the moment it activates — whether eagerly (resident) or lazily (on-demand).
        ICheckpointStore? checkpointStore = options.CheckpointDirectory is { Length: > 0 } checkpointDir
            ? new FileCheckpointStore(checkpointDir)
            : null;
        IReadOnlyDictionary<string, byte[]> restoredStates = checkpointStore is not null
            ? await checkpointStore.LoadAsync(cancellationToken)
            : new Dictionary<string, byte[]>();

        var runners = new List<INodeRunner>(executionOrder.Count);
        var runnerById = new Dictionary<string, INodeRunner>(StringComparer.OrdinalIgnoreCase);

        // Runners whose (worker-backed) state can be captured for resume-after-crash. Filled as nodes
        // activate (eagerly or lazily). A runner that reports no state is later remembered and skipped.
        var checkpointableRunners = new List<INodeRunner>();

        // Activates one node: times its warmup, registers it, and restores its persisted state (if any)
        // the moment it comes up. Shared by the eager (resident) pass and lazy on-demand activation.
        async Task<INodeRunner> ActivateNodeAsync(PipelineNodeDefinition node)
        {
            var activateStart = DateTime.UtcNow;
            var runner = await nodeActivator.ActivateAsync(node, options, cancellationToken);
            warmupByNode[node.Id] = (long)(DateTime.UtcNow - activateStart).TotalMilliseconds;
            runners.Add(runner);
            runnerById[node.Id] = runner;

            if (runner is ICheckpointable checkpointable)
            {
                checkpointableRunners.Add(runner);
                if (restoredStates.TryGetValue(node.Id, out var state))
                {
                    try { await checkpointable.RestoreAsync(state, cancellationToken); }
                    catch (Exception ex) when (ex is not OperationCanceledException)
                    {
                        warnings.Add($"Restore failed for node '{node.Id}': {ex.Message}");
                    }
                }
            }

            return runner;
        }

        // Preload resident nodes before the first cycle (models, cameras, PLCs stay warm).
        try
        {
            foreach (var node in executionOrder)
            {
                if (activationModeByNode[node.Id] != NodeActivationMode.OnDemand)
                {
                    await ActivateNodeAsync(node);
                }
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            await DisposeAllAsync(runners);
            return Failure($"Node activation failed: {ex.Message}", startedAt);
        }

        var statelessRunners = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var lastStates = new Dictionary<string, byte[]>(StringComparer.Ordinal);
        var failedOnDemand = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        // Per-node mutable stats accumulators
        var statsMap = executionOrder.ToDictionary(
            n => n.Id,
            _ => new NodeStatsAccumulator(),
            StringComparer.OrdinalIgnoreCase);

        // Cross-process counters, harvested from worker-backed runners before they are disposed (M3
        // observability): a supervisor restart is transparent to the graph, so without this the run would
        // end with no record that a child ever died.
        var workerMetricsByNode = new Dictionary<string, WorkerMetricsSnapshot>(StringComparer.OrdinalIgnoreCase);

        // Static, graph-aware data-plane routing. Inactive (heap-only, original behavior) when there is
        // no data plane or the graph has no out-of-process worker nodes.
        var workerNodeIds = DataPlaneRouter.BuildWorkerNodeIds(definition, loadedCatalog);
        var arenaActive = dataPlane is not null && workerNodeIds.Count > 0;
        var outgoingByPort = arenaActive ? DataPlaneRouter.BuildOutgoingByPort(definition) : null;

        var portBus = new GraphPortBus();
        var totalCycles = 0;
        var acceptedCycles = 0;
        var droppedFrames = 0;
        var sourceCompleted = false;
        string? backpressureFailure = null;
        string? sourceFailure = null;

        try
        {
            // Resume happens as each node activates (see ActivateNodeAsync): a resident node is restored
            // during the eager pass above, an on-demand node the moment it is first used.
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
                    var isOnDemand = activationModeByNode.GetValueOrDefault(node.Id) == NodeActivationMode.OnDemand;
                    var runnerKnown = runnerById.TryGetValue(node.Id, out var runner);

                    // An unknown node that is not a (still-viable) on-demand one can't run — resident nodes
                    // are always preloaded, so this only skips a failed-to-activate on-demand helper.
                    if (!runnerKnown && (!isOnDemand || failedOnDemand.Contains(node.Id)))
                    {
                        continue;
                    }

                    var inputs = portBus.CollectInputs(node.Id, node.Inputs, context);

                    // On-demand (short helper): only warm it up and run it when a frame actually reaches it
                    // — a gated helper costs nothing on the cycles it is idle (lazy activation + idle skip).
                    if (isOnDemand && !inputs.All.Any())
                    {
                        continue;
                    }

                    if (!runnerKnown)
                    {
                        try
                        {
                            runner = await ActivateNodeAsync(node);
                        }
                        catch (Exception ex) when (ex is not OperationCanceledException)
                        {
                            warnings.Add($"On-demand activation failed for node '{node.Id}': {ex.Message}");
                            failedOnDemand.Add(node.Id);
                            continue;
                        }
                    }

                    NodeExecutionResult result;
                    var nodeStart = Stopwatch.GetTimestamp();
                    var faulted = false;
                    string? faultMessage = null;

                    try
                    {
                        result = await runner!.ExecuteAsync(inputs, cancellationToken);
                    }
                    catch (Exception ex) when (ex is not OperationCanceledException)
                    {
                        warnings.Add($"Node '{node.Id}' threw during execution: {ex.Message}");
                        result = NodeExecutionResult.NoOutput;
                        faulted = true;
                        faultMessage = ex.Message;
                    }

                    // Raw ticks, converted once at report time: a local pipeline stage is routinely
                    // sub-millisecond, so rounding to any unit per cycle threw the cost away — whole
                    // milliseconds reported fast nodes as entirely free.
                    var nodeTicks = Stopwatch.GetTimestamp() - nodeStart;
                    if (statsMap.TryGetValue(node.Id, out var acc))
                    {
                        acc.TotalCycles++;
                        acc.TotalDurationTicks += nodeTicks;
                        if (faulted) acc.FaultedCycles++;
                    }

                    // Read the worker counters every cycle a worker node runs, so a crash absorbed by the
                    // supervisor is reported live (TUI) as well as in the final report.
                    if (runner is IWorkerMetricsSource metricsSource
                        && metricsSource.GetWorkerMetrics() is { } workerMetrics)
                    {
                        workerMetricsByNode[node.Id] = workerMetrics;
                    }

                    options.OnNodeExecuted?.Invoke(new NodeExecutionEvent
                    {
                        RunId = runId,
                        NodeId = node.Id,
                        CycleIndex = totalCycles,
                        HasOutput = result.HasOutput,
                        Faulted = faulted,
                        DurationMicros = TicksToMicros(nodeTicks),
                        WorkerRestarts = workerMetricsByNode.TryGetValue(node.Id, out var wm) ? wm.Restarts : 0,
                        OutputPortNames = result.HasOutput
                            ? result.All.Select(kvp => kvp.Key).ToList()
                            : [],
                        InputPortNames = inputs.All.Select(kvp => kvp.Key).ToList()
                    });

                    if (IsSourceNode(node) && !result.HasOutput)
                    {
                        // A source that threw is not an exhausted stream. Both look like "no frame" here,
                        // and conflating them made a camera that never connected report a clean, successful
                        // run of zero cycles. Record the failure; the run below ends unsuccessfully and,
                        // crucially, keeps its checkpoint (there is nothing "completed" to resume past).
                        if (faulted)
                        {
                            sourceFailure = $"Source node '{node.Id}' failed: {faultMessage}";
                        }

                        sourcesExhausted = true;
                        break;
                    }

                    if (IsSinkNode(node) && inputs.Has("frame"))
                    {
                        cycleHadSinkOutput = true;
                    }

                    if (arenaActive)
                    {
                        // Phase 1 — route outputs, AddRef arena buffers for the edges they now occupy.
                        // Delivery is a synchronous port-bus write here: in serial mode an edge holds at
                        // most one value, so there is never anything to wait for.
                        droppedFrames += await DataPlaneRouter.RouteAsync(
                            node.Id, result, inputs,
                            (edge, value, _) =>
                            {
                                portBus.Set(edge.To.NodeId, edge.To.Port, value);
                                return ValueTask.CompletedTask;
                            },
                            outgoingByPort!, workerNodeIds, dataPlane!,
                            backpressureByNode.GetValueOrDefault(node.Id, options.BackpressurePolicy), cancellationToken);
                        // Phase 2 — this node has run, so it no longer occupies its arena input edges.
                        DataPlaneRouter.ReleaseArenaInputs(inputs, dataPlane!);
                    }
                    else
                    {
                        portBus.RouteOutputs(node.Id, result, definition.Edges);
                    }
                }

                if (sourcesExhausted)
                {
                    sourceCompleted = sourceFailure is null;   // a failed source has not consumed its stream
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

                // Cycle boundary: the engine is quiesced, so this is a torn-free point to snapshot
                // stateful workers. A supervised worker caches the capture to recover with on a crash.
                if (options.CheckpointIntervalCycles > 0
                    && checkpointableRunners.Count > 0
                    && totalCycles % options.CheckpointIntervalCycles == 0)
                {
                    await CheckpointRunnersAsync(
                        checkpointableRunners, statelessRunners, lastStates, checkpointStore, warnings, cancellationToken);
                }
            }
        }
        catch (OperationCanceledException)
        {
            // Propagate after cleanup
        }
        catch (DataPlaneBackpressureException ex)
        {
            // A lossless (Stall) producer hit an exhausted arena, or a payload can never fit a slot.
            // Neither is recoverable mid-run: stop cleanly with an actionable message.
            backpressureFailure = ex.Message;
        }
        finally
        {
            // Final harvest before the children are shut down — disposal ends the worker processes and
            // takes their counters with them (this also catches a restart during an end-of-run checkpoint).
            foreach (var (nodeId, runner) in runnerById)
            {
                if (runner is IWorkerMetricsSource metricsSource
                    && metricsSource.GetWorkerMetrics() is { } workerMetrics)
                {
                    workerMetricsByNode[nodeId] = workerMetrics;
                }
            }

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
                TotalDurationMicros = TicksToMicros(kvp.Value.TotalDurationTicks),
                WarmupMs = warmupByNode.GetValueOrDefault(kvp.Key),
                ActivationMode = activationModeByNode.GetValueOrDefault(kvp.Key, NodeActivationMode.Resident),
                Worker = workerMetricsByNode.GetValueOrDefault(kvp.Key)
            },
            StringComparer.OrdinalIgnoreCase);

        var workerRestarts = workerMetricsByNode.Values.Sum(m => m.Restarts);

        // Either way the run stopped early for a reason the operator must see, not a clean end of stream.
        if ((backpressureFailure ?? sourceFailure) is { } runFailure)
        {
            return new PipelineExecutionReport
            {
                Succeeded = false,
                TotalCycles = totalCycles,
                AcceptedCycles = acceptedCycles,
                DroppedFrames = droppedFrames,
                WorkerRestarts = workerRestarts,
                Duration = DateTime.UtcNow - startedAt,
                ErrorMessage = runFailure,
                Warnings = warnings,
                NodeStats = nodeStats
            };
        }

        // A cleanly, fully-consumed run has nothing to resume — drop its persisted checkpoint.
        if (sourceCompleted && checkpointStore is not null)
        {
            try { await checkpointStore.ClearAsync(cancellationToken); }
            catch { /* best effort */ }
        }

        return new PipelineExecutionReport
        {
            Succeeded = true,
            TotalCycles = totalCycles,
            AcceptedCycles = acceptedCycles,
            DroppedFrames = droppedFrames,
            WorkerRestarts = workerRestarts,
            Duration = DateTime.UtcNow - startedAt,
            Warnings = warnings,
            NodeStats = nodeStats
        };
    }

    /// <summary>
    /// Resolves a node's loading profile: an explicit per-node <c>activationMode</c> wins, then the
    /// module-declared <c>lifecycle</c> default, then resident. An unparseable value (already rejected by
    /// the validator) falls back to the next source.
    /// </summary>
    private static NodeActivationMode ResolveActivationMode(
        PipelineNodeDefinition node,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog)
    {
        if (node.ActivationMode is { Length: > 0 } nodeMode && NodeActivationModes.TryParse(nodeMode, out var mode))
        {
            return mode;
        }

        if (node.ModuleId is { } id
            && catalog is not null
            && catalog.TryGetValue(id, out var entry)
            && NodeActivationModes.TryParse(entry.Manifest.Lifecycle, out var moduleMode))
        {
            return moduleMode;
        }

        return NodeActivationMode.Resident;
    }

    /// <summary>
    /// Resolves a producing node's backpressure policy: an explicit per-node <c>backpressure</c> wins, then
    /// the module-declared default, then the run-level default. An unparseable value (rejected by the
    /// validator) falls back to the next source.
    /// </summary>
    private static BackpressurePolicy ResolveBackpressurePolicy(
        PipelineNodeDefinition node,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog,
        BackpressurePolicy runDefault)
    {
        if (node.Backpressure is { Length: > 0 } nodePolicy && BackpressurePolicies.TryParse(nodePolicy, out var policy))
        {
            return policy;
        }

        if (node.ModuleId is { } id
            && catalog is not null
            && catalog.TryGetValue(id, out var entry)
            && BackpressurePolicies.TryParse(entry.Manifest.Backpressure, out var modulePolicy))
        {
            return modulePolicy;
        }

        return runDefault;
    }


    // Superseded by DataPlaneRouter.PortKey (shared with the pipelined executor). Still here only because
    // this line carries a raw NUL byte as its separator, which blocks exact-match editing — delete it by
    // hand; nothing calls it. That NUL is also why this file reads as "binary" to grep.
    private static string PortKey(string nodeId, string port) => $"{nodeId} {port}";

    /// <summary>
    /// Captures each checkpointable runner's state (best-effort) and, when a store is present, persists
    /// the accumulated states. A runner reporting no state is remembered and skipped thereafter; any
    /// failure is warned about, never fatal.
    /// </summary>
    private static async Task CheckpointRunnersAsync(
        IReadOnlyList<INodeRunner> runners,
        HashSet<string> stateless,
        Dictionary<string, byte[]> lastStates,
        ICheckpointStore? store,
        List<string> warnings,
        CancellationToken cancellationToken)
    {
        var changed = false;
        foreach (var runner in runners)
        {
            if (stateless.Contains(runner.NodeId) || runner is not ICheckpointable checkpointable)
            {
                continue;
            }

            try
            {
                var state = await checkpointable.CheckpointAsync(cancellationToken);
                if (state is null)
                {
                    stateless.Add(runner.NodeId);
                }
                else
                {
                    lastStates[runner.NodeId] = state;
                    changed = true;
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                warnings.Add($"Checkpoint failed for node '{runner.NodeId}': {ex.Message}");
            }
        }

        if (store is not null && changed && lastStates.Count > 0)
        {
            try
            {
                await store.SaveAsync(lastStates, cancellationToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                warnings.Add($"Checkpoint persist failed: {ex.Message}");
            }
        }
    }

    private static bool IsSourceNode(PipelineNodeDefinition node) => NodeRoles.IsSource(node);

    private static bool IsSinkNode(PipelineNodeDefinition node) => NodeRoles.IsSink(node);

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

    /// <summary>Stopwatch ticks → microseconds. Done in double so a long run cannot overflow the scale-up.</summary>
    private static long TicksToMicros(long ticks) => (long)(ticks * (1_000_000.0 / Stopwatch.Frequency));

    private sealed class NodeStatsAccumulator
    {
        public int TotalCycles;
        public int FaultedCycles;
        public long TotalDurationTicks;
    }
}
