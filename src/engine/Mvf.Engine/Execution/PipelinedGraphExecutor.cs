using System.Diagnostics;
using System.Threading.Channels;
using Mvf.Abstractions;
using Mvf.Engine.Modules;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Runtime;

namespace Mvf.Engine.Execution;

/// <summary>
/// Stage-parallel executor: every node runs as its own long-lived task, connected by <b>bounded per-edge
/// queues</b>. While the worker classifies frame N the source is already fetching N+1, so throughput
/// approaches the slowest single stage instead of the sum of them all.
///
/// <para><b>Backpressure is real here.</b> A full edge queue blocks its producer at the
/// <see cref="ChannelWriter{T}.WriteAsync"/>, which is the block-the-producer behaviour the serial
/// executor could only approximate by failing fast (it has no concurrent drain to wait on).</para>
///
/// <para><b>Order is preserved.</b> One task per node plus FIFO queues means a sink observes frames in
/// source order without a reorder buffer. That holds only while a node has a single instance; per-node
/// parallelism is a later slice and brings the reorder buffer with it.</para>
///
/// <para><b>Deliberately incomplete (step 1).</b> Shapes whose pipelined semantics are not built yet are
/// rejected up front rather than run with a guess — see <see cref="DescribeUnsupported"/>. Serial mode
/// remains the default and handles all of them.</para>
/// </summary>
public sealed class PipelinedGraphExecutor(
    IPipelineNodeActivator nodeActivator,
    IDataPlane? dataPlane = null,
    ModuleCatalog? moduleCatalog = null) : IPipelineGraphExecutor
{
    /// <summary>One queued value, tagged with the source cycle it belongs to.</summary>
    private readonly record struct StageMessage(long CycleId, PortValue Value);

    public async Task<PipelineExecutionReport> ExecuteAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(definition);
        ArgumentNullException.ThrowIfNull(options);

        var runId = Guid.NewGuid().ToString("N");
        var startedAt = DateTime.UtcNow;

        IReadOnlyList<PipelineNodeDefinition> executionOrder;
        try
        {
            executionOrder = GraphTopologySorter.Sort(definition);
        }
        catch (InvalidOperationException ex)
        {
            return Failure(ex.Message, startedAt);
        }

        var loadedCatalog = moduleCatalog?.Load(options.IntegrationsRoot);

        if (DescribeUnsupported(definition, executionOrder, options, loadedCatalog) is { } unsupported)
        {
            return Failure(unsupported, startedAt);
        }

        var warningLock = new object();
        var warnings = new List<string>();
        void Warn(string message) { lock (warningLock) { warnings.Add(message); } }

        var warmupByNode = new Dictionary<string, long>(StringComparer.OrdinalIgnoreCase);
        var activationModeByNode = new Dictionary<string, NodeActivationMode>(StringComparer.OrdinalIgnoreCase);
        var backpressureByNode = new Dictionary<string, BackpressurePolicy>(StringComparer.OrdinalIgnoreCase);
        foreach (var node in executionOrder)
        {
            activationModeByNode[node.Id] = ResolveActivationMode(node, loadedCatalog);
            backpressureByNode[node.Id] = ResolveBackpressurePolicy(node, loadedCatalog, options.BackpressurePolicy);
        }

        // Every node is resident here (on-demand is rejected above), so warm them all before the stages
        // start — a stage must not pay a cold start while its queue fills behind it.
        var runners = new List<INodeRunner>(executionOrder.Count);
        var runnerById = new Dictionary<string, INodeRunner>(StringComparer.OrdinalIgnoreCase);
        try
        {
            foreach (var node in executionOrder)
            {
                var activateStart = Stopwatch.GetTimestamp();
                var runner = await nodeActivator.ActivateAsync(node, options, cancellationToken);
                warmupByNode[node.Id] = (long)Stopwatch.GetElapsedTime(activateStart).TotalMilliseconds;
                runners.Add(runner);
                runnerById[node.Id] = runner;
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            await DisposeAllAsync(runners);
            return Failure($"Node activation failed: {ex.Message}", startedAt);
        }

        var workerNodeIds = DataPlaneRouter.BuildWorkerNodeIds(definition, loadedCatalog);
        var arenaActive = dataPlane is not null && workerNodeIds.Count > 0;
        var outgoingByPort = DataPlaneRouter.BuildOutgoingByPort(definition);

        // One bounded queue per edge: exactly one producer (the edge's source node) and one consumer.
        var capacity = Math.Max(1, options.EdgeQueueCapacity);
        var channelByEdge = definition.Edges.ToDictionary(
            edge => edge.Id,
            _ => Channel.CreateBounded<StageMessage>(new BoundedChannelOptions(capacity)
            {
                SingleReader = true,
                SingleWriter = true,
                FullMode = BoundedChannelFullMode.Wait   // Wait == block the producer
            }),
            StringComparer.OrdinalIgnoreCase);

        var inboundByNode = definition.Edges.ToLookup(edge => edge.To.NodeId, StringComparer.OrdinalIgnoreCase);
        var outboundByNode = definition.Edges.ToLookup(edge => edge.From.NodeId, StringComparer.OrdinalIgnoreCase);

        var statsByNode = executionOrder.ToDictionary(
            n => n.Id, _ => new NodeStatsAccumulator(), StringComparer.OrdinalIgnoreCase);

        var totalCycles = 0;
        var acceptedCycles = 0;
        var droppedFrames = 0;
        var sourceCompleted = false;
        string? runFailure = null;

        // A stage failure has to unblock every other stage, otherwise a producer stays parked on a queue
        // nobody will drain again.
        using var runCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var runToken = runCts.Token;

        void FailRun(string message)
        {
            Interlocked.CompareExchange(ref runFailure, message, null);
            runCts.Cancel();
        }

        // Routes one node's outputs into its outgoing edge queues. Writing is where a full queue blocks.
        async Task<int> RouteAsync(PipelineNodeDefinition node, NodeExecutionResult result, NodeExecutionInputs inputs, long cycleId)
        {
            if (!arenaActive)
            {
                var plain = 0;
                foreach (var (portName, value) in result.All)
                {
                    foreach (var edge in outgoingByPort.TryGetValue(DataPlaneRouter.PortKey(node.Id, portName), out var e)
                        ? e : Enumerable.Empty<PipelineEdgeDefinition>())
                    {
                        await channelByEdge[edge.Id].Writer.WriteAsync(new StageMessage(cycleId, value), runToken);
                    }
                }

                return plain;
            }

            return await DataPlaneRouter.RouteAsync(
                node.Id, result, inputs,
                async (edge, value, token) =>
                    await channelByEdge[edge.Id].Writer.WriteAsync(new StageMessage(cycleId, value), token),
                outgoingByPort, workerNodeIds, dataPlane!,
                backpressureByNode.GetValueOrDefault(node.Id, options.BackpressurePolicy), runToken);
        }

        // Runs one node once and books its timing/fault, returning the result (NoOutput when it threw).
        async Task<(NodeExecutionResult Result, bool Faulted, string? Message)> RunOnceAsync(
            PipelineNodeDefinition node, INodeRunner runner, NodeExecutionInputs inputs)
        {
            var start = Stopwatch.GetTimestamp();
            NodeExecutionResult result;
            var faulted = false;
            string? message = null;
            try
            {
                result = await runner.ExecuteAsync(inputs, runToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                Warn($"Node '{node.Id}' threw during execution: {ex.Message}");
                result = NodeExecutionResult.NoOutput;
                faulted = true;
                message = ex.Message;
            }

            var acc = statsByNode[node.Id];
            acc.TotalCycles++;
            acc.TotalDurationTicks += Stopwatch.GetTimestamp() - start;
            if (faulted) acc.FaultedCycles++;

            var runner_ = runner;   // metrics are read on the stage's own thread, so no shared-state race
            if (runner_ is IWorkerMetricsSource metrics && metrics.GetWorkerMetrics() is { } snapshot)
            {
                acc.Worker = snapshot;
            }

            options.OnNodeExecuted?.Invoke(new NodeExecutionEvent
            {
                RunId = runId,
                NodeId = node.Id,
                CycleIndex = (int)Math.Min(int.MaxValue, inputs.Context?.CycleIndex ?? 0),
                HasOutput = result.HasOutput,
                Faulted = faulted,
                DurationMicros = TicksToMicros(Stopwatch.GetTimestamp() - start),
                WorkerRestarts = acc.Worker?.Restarts ?? 0,
                OutputPortNames = result.HasOutput ? result.All.Select(kvp => kvp.Key).ToList() : [],
                InputPortNames = inputs.All.Select(kvp => kvp.Key).ToList()
            });

            return (result, faulted, message);
        }

        // The source stage drives the run: it produces until exhausted, cancelled, or capped by MaxCycles.
        async Task RunSourceStageAsync(PipelineNodeDefinition node, INodeRunner runner)
        {
            long cycle = 0;
            try
            {
                while (!runToken.IsCancellationRequested)
                {
                    if (options.MaxCycles > 0 && cycle >= options.MaxCycles)
                    {
                        break;
                    }

                    var context = new NodeExecutionContext
                    {
                        RunId = runId,
                        CycleIndex = (int)Math.Min(int.MaxValue, cycle),
                        CycleStartedAt = DateTime.UtcNow
                    };
                    var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>(), context);

                    var (result, faulted, message) = await RunOnceAsync(node, runner, inputs);
                    if (!result.HasOutput)
                    {
                        // Same distinction the serial executor makes: a source that threw is a failure,
                        // a source that simply ran dry is a clean end of stream.
                        if (faulted)
                        {
                            FailRun($"Source node '{node.Id}' failed: {message}");
                        }
                        else
                        {
                            sourceCompleted = true;
                        }

                        break;
                    }

                    Interlocked.Add(ref droppedFrames, await RouteAsync(node, result, inputs, cycle));

                    cycle++;
                    Interlocked.Exchange(ref totalCycles, (int)Math.Min(int.MaxValue, cycle));
                    options.OnCycleCompleted?.Invoke(new PipelineExecutionProgress
                    {
                        RunId = runId,
                        CycleIndex = (int)Math.Min(int.MaxValue, cycle - 1),
                        TotalCycles = totalCycles,
                        AcceptedCycles = Volatile.Read(ref acceptedCycles),
                        CycleAccepted = true,
                        Elapsed = DateTime.UtcNow - startedAt
                    });
                }
            }
            finally
            {
                CompleteOutputs(node);
            }
        }

        // Every other stage consumes its single input queue until the upstream completes it.
        async Task RunConsumerStageAsync(PipelineNodeDefinition node, INodeRunner runner, PipelineEdgeDefinition inboundEdge)
        {
            var isSink = NodeRoles.IsSink(node);
            try
            {
                await foreach (var message in channelByEdge[inboundEdge.Id].Reader.ReadAllAsync(runToken))
                {
                    var context = new NodeExecutionContext
                    {
                        RunId = runId,
                        CycleIndex = (int)Math.Min(int.MaxValue, message.CycleId),
                        CycleStartedAt = DateTime.UtcNow
                    };
                    var inputs = new NodeExecutionInputs(
                        new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
                        {
                            [inboundEdge.To.Port] = message.Value
                        },
                        context);

                    var (result, _, _) = await RunOnceAsync(node, runner, inputs);

                    if (isSink)
                    {
                        Interlocked.Increment(ref acceptedCycles);
                    }

                    Interlocked.Add(ref droppedFrames, await RouteAsync(node, result, inputs, message.CycleId));

                    // This node has run, so it no longer occupies its arena input edge.
                    if (arenaActive)
                    {
                        DataPlaneRouter.ReleaseArenaInputs(inputs, dataPlane!);
                    }
                }
            }
            finally
            {
                CompleteOutputs(node);
            }
        }

        void CompleteOutputs(PipelineNodeDefinition node)
        {
            foreach (var edge in outboundByNode[node.Id])
            {
                channelByEdge[edge.Id].Writer.TryComplete();
            }
        }

        var workerMetricsByNode = new Dictionary<string, WorkerMetricsSnapshot>(StringComparer.OrdinalIgnoreCase);
        try
        {
            var stages = new List<Task>(executionOrder.Count);
            foreach (var node in executionOrder)
            {
                var runner = runnerById[node.Id];
                var inbound = inboundByNode[node.Id].FirstOrDefault();
                stages.Add(Task.Run(async () =>
                {
                    try
                    {
                        if (inbound is null)
                        {
                            await RunSourceStageAsync(node, runner);
                        }
                        else
                        {
                            await RunConsumerStageAsync(node, runner, inbound);
                        }
                    }
                    catch (OperationCanceledException)
                    {
                        // Cancellation is how a failing stage stops the others; not itself a failure.
                    }
                    catch (DataPlaneBackpressureException ex)
                    {
                        FailRun(ex.Message);
                    }
                    catch (Exception ex)
                    {
                        FailRun($"Stage '{node.Id}' failed: {ex.Message}");
                    }
                }, CancellationToken.None));
            }

            await Task.WhenAll(stages);
        }
        finally
        {
            foreach (var (nodeId, runner) in runnerById)
            {
                if (runner is IWorkerMetricsSource metricsSource && metricsSource.GetWorkerMetrics() is { } metrics)
                {
                    workerMetricsByNode[nodeId] = metrics;
                }
            }

            await DisposeAllAsync(runners);
        }

        cancellationToken.ThrowIfCancellationRequested();

        var nodeStats = statsByNode.ToDictionary(
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

        _ = sourceCompleted;   // pipelined mode has no checkpoint store to clear yet (step 2)

        return new PipelineExecutionReport
        {
            Succeeded = runFailure is null,
            TotalCycles = totalCycles,
            AcceptedCycles = acceptedCycles,
            DroppedFrames = droppedFrames,
            WorkerRestarts = workerMetricsByNode.Values.Sum(m => m.Restarts),
            Duration = DateTime.UtcNow - startedAt,
            ErrorMessage = runFailure,
            Warnings = warnings,
            NodeStats = nodeStats
        };
    }

    /// <summary>
    /// Graph and option shapes this executor cannot yet run correctly, each with the reason and the way
    /// out. Returning null means the graph is supported. Refusing beats guessing: a mis-paired join or a
    /// silently skipped checkpoint would be far more expensive to discover later.
    /// </summary>
    private static string? DescribeUnsupported(
        PipelineDefinition definition,
        IReadOnlyList<PipelineNodeDefinition> executionOrder,
        PipelineExecutionOptions options,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog)
    {
        if (options.CheckpointIntervalCycles > 0 || options.CheckpointDirectory is { Length: > 0 })
        {
            return "Pipelined mode does not support checkpointing yet — there is no quiesced cycle boundary "
                 + "to snapshot at until epoch barriers land. Run this pipeline in serial mode.";
        }

        foreach (var node in executionOrder)
        {
            var inbound = definition.Edges.Count(e =>
                StringComparer.OrdinalIgnoreCase.Equals(e.To.NodeId, node.Id));
            if (inbound > 1)
            {
                return $"Node '{node.Id}' has {inbound} incoming edges. Pipelined mode cannot join several "
                     + "inputs yet — that needs correlation by cycle id plus markers for branches that "
                     + "produce nothing. Run this pipeline in serial mode.";
            }

            if (ResolveActivationMode(node, catalog) == NodeActivationMode.OnDemand)
            {
                return $"Node '{node.Id}' is on-demand. Lazy activation is serial-only for now; in pipelined "
                     + "mode every stage is warmed before the run. Run this pipeline in serial mode.";
            }
        }

        return null;
    }

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

    private static long TicksToMicros(long ticks) => (long)(ticks * (1_000_000.0 / Stopwatch.Frequency));

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
            try { await runner.DisposeAsync(); } catch { /* best effort */ }
        }
    }

    private sealed class NodeStatsAccumulator
    {
        public int TotalCycles;
        public int FaultedCycles;
        public long TotalDurationTicks;
        public WorkerMetricsSnapshot? Worker;
    }
}
