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
        var workerNodeIds = BuildWorkerNodeIds(definition, loadedCatalog);
        var arenaActive = dataPlane is not null && workerNodeIds.Count > 0;
        var outgoingByPort = arenaActive ? BuildOutgoingByPort(definition) : null;

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
                        droppedFrames += await RouteOutputsWithDataPlaneAsync(
                            node.Id, result, inputs, portBus, outgoingByPort!, workerNodeIds, dataPlane!,
                            backpressureByNode.GetValueOrDefault(node.Id, options.BackpressurePolicy), cancellationToken);
                        // Phase 2 — this node has run, so it no longer occupies its arena input edges.
                        ReleaseArenaInputs(inputs, dataPlane!);
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

    /// <summary>Node ids whose module runs out-of-process (a non-<c>dotnet</c> runtime in the catalog).</summary>
    private static IReadOnlySet<string> BuildWorkerNodeIds(
        PipelineDefinition definition,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog)
    {
        var workers = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        if (catalog is null)
        {
            return workers;
        }

        foreach (var node in definition.Nodes)
        {
            if (node.ModuleId is not null
                && catalog.TryGetValue(node.ModuleId, out var entry)
                && !string.Equals(entry.Manifest.Runtime, "dotnet", StringComparison.OrdinalIgnoreCase))
            {
                workers.Add(node.Id);
            }
        }

        return workers;
    }

    private static Dictionary<string, List<PipelineEdgeDefinition>> BuildOutgoingByPort(PipelineDefinition definition)
    {
        var map = new Dictionary<string, List<PipelineEdgeDefinition>>(StringComparer.OrdinalIgnoreCase);
        foreach (var edge in definition.Edges)
        {
            var key = PortKey(edge.From.NodeId, edge.From.Port);
            if (!map.TryGetValue(key, out var list))
            {
                list = [];
                map[key] = list;
            }

            list.Add(edge);
        }

        return map;
    }

    private static string PortKey(string nodeId, string port) => $"{nodeId} {port}";

    /// <summary>
    /// Routes a node's outputs with graph-aware transport selection and <b>live-edge-occupancy</b>
    /// reference counting — a buffer's refcount equals the number of edges currently carrying it.
    /// <list type="bullet">
    ///   <item><b>Heap frame</b> that fans out to workers: published into the arena once (refcount =
    ///   worker edges); the arena handle goes to workers, in-process consumers keep the heap frame.</item>
    ///   <item><b>Arena frame</b> (a worker's output, or a pass-through node re-emitting an arena input):
    ///   every consumer reads the arena, so it is <see cref="IDataPlane.AddRef"/>'d by the number of
    ///   outgoing edges and delivered to all of them.</item>
    ///   <item>Control signals and in-process-only frames are routed by reference, unchanged.</item>
    /// </list>
    /// A newly produced arena buffer (not one of this node's inputs) carries a producer hold from
    /// reservation; once routed, that hold is dropped so its refcount is exactly its live edge count.
    /// AddRef runs here (Phase 1), before the caller releases this node's arena inputs (Phase 2), so a
    /// forwarded buffer never transiently reaches zero.
    /// </summary>
    private static async Task<int> RouteOutputsWithDataPlaneAsync(
        string sourceNodeId,
        NodeExecutionResult result,
        NodeExecutionInputs inputs,
        GraphPortBus portBus,
        IReadOnlyDictionary<string, List<PipelineEdgeDefinition>> outgoingByPort,
        IReadOnlySet<string> workerNodeIds,
        IDataPlane dataPlane,
        BackpressurePolicy policy,
        CancellationToken cancellationToken)
    {
        var dropped = 0;

        // Arena buffers this node received as inputs — an output carrying one of these is a forwarded
        // pass-through, not a newly produced buffer, so it keeps no producer hold to drop.
        var inputArenaHandles = new HashSet<ArenaHandle>();
        foreach (var (_, value) in inputs.All)
        {
            if (value.Frame is ArenaFrameEnvelope arenaInput)
            {
                inputArenaHandles.Add(arenaInput.Handle);
            }
        }

        var producedArenaHandles = new HashSet<ArenaHandle>();

        foreach (var (portName, value) in result.All)
        {
            var edges = outgoingByPort.TryGetValue(PortKey(sourceNodeId, portName), out var e) ? e : null;

            if (value.Frame is ArenaFrameEnvelope arenaFrame)
            {
                // Arena-born or forwarded: every consumer reads the arena in place.
                var edgeCount = edges?.Count ?? 0;
                dataPlane.AddRef(arenaFrame.Handle, edgeCount);
                if (edges is not null)
                {
                    foreach (var edge in edges)
                    {
                        portBus.Set(edge.To.NodeId, edge.To.Port, value);
                    }
                }

                producedArenaHandles.Add(arenaFrame.Handle);
                continue;
            }

            if (edges is null)
            {
                continue;
            }

            if (value.Frame is not null)
            {
                var workerEdgeCount = edges.Count(edge => workerNodeIds.Contains(edge.To.NodeId));
                if (workerEdgeCount > 0)
                {
                    var (outcome, arenaValue) = await TryPublishFrameForWorkersAsync(
                        value.Frame, workerEdgeCount, dataPlane, cancellationToken);

                    if (outcome == PublishOutcome.Published)
                    {
                        foreach (var edge in edges)
                        {
                            var deliver = workerNodeIds.Contains(edge.To.NodeId) ? arenaValue! : value;
                            portBus.Set(edge.To.NodeId, edge.To.Port, deliver);
                        }

                        // Published with refcount == worker-edge count (no producer hold), so each worker
                        // release balances it — this buffer is not tracked for a producer-hold drop.
                        continue;
                    }

                    if (outcome == PublishOutcome.PayloadTooLarge)
                    {
                        // Not backpressure: a frame that never fits a slot can't be fixed by waiting or
                        // by dropping every frame forever. Stop with an actionable message under any policy.
                        throw new DataPlaneBackpressureException(
                            $"Frame on '{sourceNodeId}.{portName}' exceeds the arena slot capacity " +
                            $"({dataPlane.SlotSize} bytes) and can never be published — increase the slot size.");
                    }

                    // Arena momentarily full → the lossless-vs-lossy choice.
                    if (policy == BackpressurePolicy.Stall)
                    {
                        throw new DataPlaneBackpressureException(
                            $"Data plane full while publishing '{sourceNodeId}.{portName}' to {workerEdgeCount} " +
                            "worker edge(s); a lossless (stall) run cannot proceed — raise the arena slot count " +
                            "or set the backpressure policy to drop.");
                    }

                    // Drop: out-of-process consumers miss this frame; in-process consumers on the same
                    // output still receive the heap frame. The source keeps running (bounded latency).
                    foreach (var edge in edges)
                    {
                        if (!workerNodeIds.Contains(edge.To.NodeId))
                        {
                            portBus.Set(edge.To.NodeId, edge.To.Port, value);
                        }
                    }

                    dropped++;
                    continue;
                }
            }

            foreach (var edge in edges)
            {
                portBus.Set(edge.To.NodeId, edge.To.Port, value);
            }
        }

        // Drop the producer hold on buffers born at this node (a worker's output). Forwarded input
        // buffers keep their occupancy and are released in Phase 2 instead.
        foreach (var handle in producedArenaHandles)
        {
            if (!inputArenaHandles.Contains(handle))
            {
                dataPlane.Release(handle);
            }
        }

        return dropped;
    }

    /// <summary>Why a publish attempt did not place a frame in the arena.</summary>
    private enum PublishOutcome
    {
        /// <summary>The frame is in the arena; the returned value carries its handle.</summary>
        Published,

        /// <summary>The arena is momentarily full — every slot carries a live buffer (backpressure).</summary>
        ArenaFull,

        /// <summary>The frame is larger than a slot and can never be published (a sizing error).</summary>
        PayloadTooLarge
    }

    /// <summary>
    /// Copies a heap frame into the arena once (refcount = its worker-edge count) and, on failure,
    /// classifies whether the arena was merely full (backpressure) or the frame can never fit a slot.
    /// The caller turns that classification into the configured policy.
    /// </summary>
    private static async Task<(PublishOutcome Outcome, PortValue? Value)> TryPublishFrameForWorkersAsync(
        IFrameEnvelope frame,
        int referenceCount,
        IDataPlane dataPlane,
        CancellationToken cancellationToken)
    {
        byte[] bytes;
        await using (var stream = await frame.OpenReadAsync(cancellationToken))
        using (var buffer = new MemoryStream())
        {
            await stream.CopyToAsync(buffer, cancellationToken);
            bytes = buffer.ToArray();
        }

        // An encoded frame is an opaque byte blob (u8, length N); its media type/decoding is the
        // consumer's concern. Raw tensors get a richer descriptor when those payload types land.
        var descriptor = new PayloadDescriptor(PayloadMediaType.Blob, PayloadElementType.UInt8, [bytes.Length]);
        if (dataPlane.TryPublish(descriptor, bytes, referenceCount, out var handle))
        {
            return (PublishOutcome.Published, PortValue.FromFrame(new ArenaFrameEnvelope(dataPlane, handle, frame)));
        }

        // A payload that can never fit a slot is a permanent sizing error, not transient backpressure.
        var tooLarge = PayloadDescriptor.HeaderSize + (long)bytes.Length > dataPlane.SlotSize;
        return (tooLarge ? PublishOutcome.PayloadTooLarge : PublishOutcome.ArenaFull, null);
    }

    /// <summary>Releases one reference for each arena-backed frame this node consumed, now that it has run.</summary>
    private static void ReleaseArenaInputs(NodeExecutionInputs inputs, IDataPlane dataPlane)
    {
        foreach (var (_, value) in inputs.All)
        {
            if (value.Frame is ArenaFrameEnvelope arenaFrame)
            {
                dataPlane.Release(arenaFrame.Handle);
            }
        }
    }

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

    /// <summary>Stopwatch ticks → microseconds. Done in double so a long run cannot overflow the scale-up.</summary>
    private static long TicksToMicros(long ticks) => (long)(ticks * (1_000_000.0 / Stopwatch.Frequency));

    private sealed class NodeStatsAccumulator
    {
        public int TotalCycles;
        public int FaultedCycles;
        public long TotalDurationTicks;
    }

    /// <summary>
    /// Thrown when a producer cannot place a frame in the arena and the run cannot continue: a lossless
    /// (stall) policy meeting an exhausted arena, or a frame that can never fit a slot. Caught by the
    /// executor and turned into a failed report with an actionable message.
    /// </summary>
    private sealed class DataPlaneBackpressureException(string message) : Exception(message);
}
