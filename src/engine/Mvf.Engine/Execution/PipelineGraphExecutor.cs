using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Modules;
using Mvf.Engine.Recovery;

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

        // Runners whose (worker-backed) state can be captured for resume-after-crash. A runner that
        // reports no state is remembered and skipped so we don't keep paying a round-trip for it.
        var checkpointableRunners = runners.Where(r => r is ICheckpointable).ToList();
        var statelessRunners = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var lastStates = new Dictionary<string, byte[]>(StringComparer.Ordinal);

        // Durable checkpoints (engine-crash resume) when a directory is configured; otherwise captures
        // stay in memory (worker-crash recovery only).
        ICheckpointStore? checkpointStore = options.CheckpointDirectory is { Length: > 0 } checkpointDir
            ? new FileCheckpointStore(checkpointDir)
            : null;

        // Per-node mutable stats accumulators
        var statsMap = executionOrder.ToDictionary(
            n => n.Id,
            _ => new NodeStatsAccumulator(),
            StringComparer.OrdinalIgnoreCase);

        // Static, graph-aware data-plane routing. Inactive (heap-only, original behavior) when there is
        // no data plane or the graph has no out-of-process worker nodes.
        var workerNodeIds = BuildWorkerNodeIds(definition, moduleCatalog, options);
        var arenaActive = dataPlane is not null && workerNodeIds.Count > 0;
        var outgoingByPort = arenaActive ? BuildOutgoingByPort(definition) : null;

        var portBus = new GraphPortBus();
        var totalCycles = 0;
        var acceptedCycles = 0;
        var sourceCompleted = false;

        try
        {
            // Resume: restore each node's persisted state (incl. a checkpointable source's position)
            // before the first cycle, so an interrupted run continues where it left off.
            if (checkpointStore is not null)
            {
                var restored = await checkpointStore.LoadAsync(cancellationToken);
                foreach (var runner in checkpointableRunners)
                {
                    if (restored.TryGetValue(runner.NodeId, out var state) && runner is ICheckpointable checkpointable)
                    {
                        try { await checkpointable.RestoreAsync(state, cancellationToken); }
                        catch (Exception ex) when (ex is not OperationCanceledException)
                        {
                            warnings.Add($"Restore failed for node '{runner.NodeId}': {ex.Message}");
                        }
                    }
                }
            }

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

                    if (arenaActive)
                    {
                        // Phase 1 — route outputs, AddRef arena buffers for the edges they now occupy.
                        await RouteOutputsWithDataPlaneAsync(
                            node.Id, result, inputs, portBus, outgoingByPort!, workerNodeIds, dataPlane!, cancellationToken);
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
                    sourceCompleted = true;
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
        finally
        {
            await DisposeAllAsync(runners);
        }

        cancellationToken.ThrowIfCancellationRequested();

        // A cleanly, fully-consumed run has nothing to resume — drop its persisted checkpoint.
        if (sourceCompleted && checkpointStore is not null)
        {
            try { await checkpointStore.ClearAsync(cancellationToken); }
            catch { /* best effort */ }
        }

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

    /// <summary>Node ids whose module runs out-of-process (a non-<c>dotnet</c> runtime in the catalog).</summary>
    private static IReadOnlySet<string> BuildWorkerNodeIds(
        PipelineDefinition definition,
        ModuleCatalog? moduleCatalog,
        PipelineExecutionOptions options)
    {
        var workers = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        if (moduleCatalog is null)
        {
            return workers;
        }

        var catalog = moduleCatalog.Load(options.IntegrationsRoot);
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
    private static async Task RouteOutputsWithDataPlaneAsync(
        string sourceNodeId,
        NodeExecutionResult result,
        NodeExecutionInputs inputs,
        GraphPortBus portBus,
        IReadOnlyDictionary<string, List<PipelineEdgeDefinition>> outgoingByPort,
        IReadOnlySet<string> workerNodeIds,
        IDataPlane dataPlane,
        CancellationToken cancellationToken)
    {
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
                if (workerEdgeCount > 0
                    && await TryPublishFrameAsync(value.Frame, workerEdgeCount, dataPlane, cancellationToken) is { } arenaValue)
                {
                    foreach (var edge in edges)
                    {
                        var deliver = workerNodeIds.Contains(edge.To.NodeId) ? arenaValue : value;
                        portBus.Set(edge.To.NodeId, edge.To.Port, deliver);
                    }

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
    }

    private static async Task<PortValue?> TryPublishFrameAsync(
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
        return dataPlane.TryPublish(descriptor, bytes, referenceCount, out var handle)
            ? PortValue.FromFrame(new ArenaFrameEnvelope(dataPlane, handle, frame))
            : null;
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

    private sealed class NodeStatsAccumulator
    {
        public int TotalCycles;
        public int FaultedCycles;
        public long TotalDurationMs;
    }
}
