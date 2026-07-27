using System.Diagnostics;
using System.Threading.Channels;
using Mvf.Abstractions;
using Mvf.Engine.Modules;
using Mvf.Engine.Recovery;
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
    /// <summary>
    /// One queued value, tagged with the source cycle it belongs to. A null <paramref name="Value"/> is a
    /// <b>void marker</b>: "this edge produced nothing for this cycle". Every edge carries exactly one
    /// message per cycle, which is what lets a join pair its inputs by position and never deadlock waiting
    /// on a branch that was not taken.
    /// </summary>
    private readonly record struct StageMessage(long CycleId, PortValue? Value);

    /// <summary>The moving parts of a replicated node: a shared work queue, N executors, one ordering emitter.</summary>
    private sealed record ParallelStage(
        Channel<(long CycleId, NodeExecutionInputs Inputs)> Work,
        Channel<(long CycleId, NodeExecutionResult Result, NodeExecutionInputs? Inputs)> Done,
        IReadOnlyList<Task> Executors,
        Task Emitter);

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

        ICheckpointStore? checkpointStore = options.CheckpointDirectory is { Length: > 0 } checkpointDir
            ? new FileCheckpointStore(checkpointDir)
            : null;
        IReadOnlyDictionary<string, byte[]> restoredStates = checkpointStore is not null
            ? await checkpointStore.LoadAsync(cancellationToken)
            : new Dictionary<string, byte[]>();

        // Every node is resident here (on-demand is rejected above), so warm them all before the stages
        // start — a stage must not pay a cold start while its queue fills behind it. Restoring here, before
        // any stage runs, is the pipelined equivalent of serial's restore-before-cycle-0.
        var runners = new List<INodeRunner>(executionOrder.Count);
        var runnerById = new Dictionary<string, INodeRunner>(StringComparer.OrdinalIgnoreCase);
        var instancesByNode = new Dictionary<string, List<INodeRunner>>(StringComparer.OrdinalIgnoreCase);
        foreach (var node in executionOrder)
        {
            var instanceCount = ResolveParallelism(node, loadedCatalog);
            var activateStart = Stopwatch.GetTimestamp();

            try
            {
                // One activation per instance: the activator spawns a worker per call, so N instances are
                // N independent processes — a crash still costs exactly one frame and SupervisedWorker
                // recovers it unchanged.
                var instances = new List<INodeRunner>(instanceCount);
                for (var i = 0; i < instanceCount; i++)
                {
                    instances.Add(await nodeActivator.ActivateAsync(node, options, cancellationToken));
                }

                warmupByNode[node.Id] = (long)Stopwatch.GetElapsedTime(activateStart).TotalMilliseconds;
                runners.AddRange(instances);
                instancesByNode[node.Id] = instances;
                runnerById[node.Id] = instances[0];

                // Only instance 0 carries state — a replicated node is stateless by contract, so there is
                // nothing to restore into the others.
                await CheckpointCoordinator.RestoreAsync(instances[0], restoredStates, Warn, cancellationToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                // Name the node and module and flatten the inner cause (worker startup carries the
                // child's stderr) so the failure is actionable, not a bare "Node activation failed:".
                await DisposeAllAsync(runners);
                return Failure(ExecutionDiagnostics.ActivationFailure(node, ex), startedAt);
            }
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

        // In-process module logging: each node's (level, message) sink, built once. A stage points the
        // ambient holder at its node's sink once at stage start (see RunSourceStageAsync /
        // RunConsumerStageAsync) — since a stage is its own async flow, that is one async-local write per
        // stage and zero per-cycle allocation. Null when nothing observes logs.
        var moduleLogSinks = options.OnNodeLog is { } onNodeLog
            ? executionOrder.ToDictionary(
                n => n.Id, n => ExecutionDiagnostics.NodeLogSink(n, onNodeLog), StringComparer.OrdinalIgnoreCase)
            : null;

        void EnterModuleLog(PipelineNodeDefinition node)
        {
            if (moduleLogSinks is not null)
            {
                ModuleLogContext.Enter().Sink = moduleLogSinks.GetValueOrDefault(node.Id);
            }
        }

        // A stage failure has to unblock every other stage, otherwise a producer stays parked on a queue
        // nobody will drain again.
        using var runCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var runToken = runCts.Token;

        void FailRun(string message)
        {
            Interlocked.CompareExchange(ref runFailure, message, null);
            runCts.Cancel();
        }

        // AcceptedCycles means "cycles where at least one sink received output" — a property of the cycle,
        // not of a sink. Counting sink executions instead would report 8 for a 4-frame run with three
        // sinks, i.e. the same report field meaning something different per mode. Every sink sees every
        // cycle exactly once (void markers guarantee it), so a cycle is decided once all sinks report and
        // its entry can be dropped — bounded memory, no growing set of ids.
        // ── Epoch barrier ────────────────────────────────────────────────────────────────────────────
        // A pipelined run has no naturally quiesced moment, and M2.5's whole guarantee is that a capture
        // happens when nothing is in flight. So the source periodically stops and waits for the pipeline
        // to drain. "Drained" is decided at the leaves: every node reaches some leaf, edges are FIFO and
        // carry one message per cycle, so once every leaf has finished cycle C every node upstream has
        // too — no frame in flight, and every arena input released. Cheaper than aligned barriers and it
        // keeps the existing checkpoint contract literally true rather than redefining it.
        var leafNodeIds = executionOrder
            .Where(n => !outboundByNode[n.Id].Any())
            .Select(n => n.Id)
            .ToList();
        var drainLock = new object();
        var lastCycleByLeaf = leafNodeIds.ToDictionary(id => id, _ => -1L, StringComparer.OrdinalIgnoreCase);
        var drainTarget = -1L;
        TaskCompletionSource? drainSignal = null;

        void NoteLeafCompleted(string nodeId, long cycleId)
        {
            lock (drainLock)
            {
                if (!lastCycleByLeaf.ContainsKey(nodeId))
                {
                    return;
                }

                lastCycleByLeaf[nodeId] = cycleId;
                if (drainSignal is not null && lastCycleByLeaf.Values.All(seen => seen >= drainTarget))
                {
                    drainSignal.TrySetResult();
                    drainSignal = null;
                }
            }
        }

        Task WaitForDrainAsync(long throughCycleId)
        {
            lock (drainLock)
            {
                if (lastCycleByLeaf.Count == 0 || lastCycleByLeaf.Values.All(seen => seen >= throughCycleId))
                {
                    return Task.CompletedTask;
                }

                drainTarget = throughCycleId;
                drainSignal = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
                return drainSignal.Task;
            }
        }

        var statelessRunners = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var lastStates = new Dictionary<string, byte[]>(StringComparer.Ordinal);

        var sinkStageCount = executionOrder.Count(NodeRoles.IsSink);
        var acceptedLock = new object();
        var sinkReportsByCycle = new Dictionary<long, (int Reports, bool Accepted)>();

        void ReportSinkCycle(long cycleId, bool accepted)
        {
            lock (acceptedLock)
            {
                sinkReportsByCycle.TryGetValue(cycleId, out var entry);
                entry = (entry.Reports + 1, entry.Accepted || accepted);

                if (entry.Reports >= sinkStageCount)
                {
                    sinkReportsByCycle.Remove(cycleId);
                    if (entry.Accepted)
                    {
                        acceptedCycles++;
                    }
                }
                else
                {
                    sinkReportsByCycle[cycleId] = entry;
                }
            }
        }

        // Routes one node's outputs into its outgoing edge queues, splitting the cost into publish/deliver
        // work and time actually parked on a full queue — the two say very different things about a
        // bottleneck, so they are never mixed.
        async Task<int> RouteAsync(PipelineNodeDefinition node, NodeExecutionResult result, NodeExecutionInputs inputs, long cycleId)
        {
            var acc = statsByNode[node.Id];
            var routeStart = Stopwatch.GetTimestamp();
            var writeTicks = 0L;
            var written = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

            async ValueTask WriteAsync(PipelineEdgeDefinition edge, PortValue value, CancellationToken token)
            {
                var writeStart = Stopwatch.GetTimestamp();
                await channelByEdge[edge.Id].Writer.WriteAsync(new StageMessage(cycleId, value), token);
                writeTicks += Stopwatch.GetTimestamp() - writeStart;
                written.Add(edge.Id);
            }

            var dropped = 0;
            if (!arenaActive)
            {
                foreach (var (portName, value) in result.All)
                {
                    foreach (var edge in outgoingByPort.TryGetValue(DataPlaneRouter.PortKey(node.Id, portName), out var e)
                        ? e : Enumerable.Empty<PipelineEdgeDefinition>())
                    {
                        await WriteAsync(edge, value, runToken);
                    }
                }
            }
            else
            {
                dropped = await DataPlaneRouter.RouteAsync(
                    node.Id, result, inputs, WriteAsync,
                    outgoingByPort, workerNodeIds, dataPlane!,
                    backpressureByNode.GetValueOrDefault(node.Id, options.BackpressurePolicy), runToken);
            }

            // Keep every outgoing edge at exactly one message for this cycle. An untaken switch branch, a
            // transformer that dropped the frame, or a worker edge skipped under the Drop policy all send
            // a void marker instead of nothing — otherwise a downstream join would wait for a value that
            // is never coming, and the edges would drift out of step for every later cycle too.
            foreach (var edge in outboundByNode[node.Id])
            {
                if (!written.Contains(edge.Id))
                {
                    var writeStart = Stopwatch.GetTimestamp();
                    await channelByEdge[edge.Id].Writer.WriteAsync(new StageMessage(cycleId, null), runToken);
                    writeTicks += Stopwatch.GetTimestamp() - writeStart;
                }
            }

            acc.WriteBlockedTicks += writeTicks;
            acc.RouteTicks += Stopwatch.GetTimestamp() - routeStart - writeTicks;
            return dropped;
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
                Warn(ExecutionDiagnostics.ExecutionFailure(node, ex));
                result = NodeExecutionResult.NoOutput;
                faulted = true;
                message = ExecutionDiagnostics.Flatten(ex);
            }

            // A replicated node has N executors sharing this accumulator.
            var acc = statsByNode[node.Id];
            Interlocked.Increment(ref acc.TotalCycles);
            Interlocked.Add(ref acc.TotalDurationTicks, Stopwatch.GetTimestamp() - start);
            if (faulted) Interlocked.Increment(ref acc.FaultedCycles);

            var runner_ = runner;   // metrics are read on the stage's own thread, so no shared-state race
            if (runner_ is IWorkerMetricsSource metrics && metrics.GetWorkerMetrics() is { } snapshot)
            {
                acc.Worker = snapshot;
            }

            // A source's frame arrived on a producer thread, so its receive/queue/wait never shows in the
            // node span above. Poll it exactly as the worker metrics are polled — once per produced frame.
            FrameAcquisitionSample? acquisition = null;
            if (result.HasOutput
                && runner_ is ISourceAcquisitionMetrics sourceMetrics
                && sourceMetrics.GetLastAcquisition() is { } sample)
            {
                (acc.Source ??= new SourceAcquisitionAccumulator()).Record(sample);
                acquisition = sample;
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
                Acquisition = acquisition,
                OutputPortNames = result.HasOutput ? result.All.Select(kvp => kvp.Key).ToList() : [],
                InputPortNames = inputs.All.Select(kvp => kvp.Key).ToList()
            });

            return (result, faulted, message);
        }

        // The source stage drives the run: it produces until exhausted, cancelled, or capped by MaxCycles.
        async Task RunSourceStageAsync(PipelineNodeDefinition node, INodeRunner runner)
        {
            // One async-local write for this stage's whole life; every RunOnceAsync below inherits it.
            EnterModuleLog(node);
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

                    // Epoch barrier: stop feeding, let the pipeline drain, snapshot while nothing is in
                    // flight, then resume. The stall is the price of keeping the capture torn-free.
                    if (options.CheckpointIntervalCycles > 0 && cycle % options.CheckpointIntervalCycles == 0)
                    {
                        await WaitForDrainAsync(cycle - 1).WaitAsync(runToken);
                        await CheckpointCoordinator.CaptureAsync(
                            runners, statelessRunners, lastStates, checkpointStore, Warn, runToken);
                    }
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

        // Every other stage joins its inbound edges: one message from each, all belonging to the same
        // cycle, then run once. Because every edge carries exactly one message per cycle, reading one from
        // each pairs them by construction — no correlation buffer, no head-of-line blocking.
        async Task RunConsumerStageAsync(
            PipelineNodeDefinition node,
            IReadOnlyList<INodeRunner> instances,
            IReadOnlyList<PipelineEdgeDefinition> inboundEdges)
        {
            // One async-local write for this stage's whole life; every RunOnceAsync below — including the
            // per-instance Task.Run executors — inherits it by capturing this flow's ExecutionContext.
            EnterModuleLog(node);
            var isSink = NodeRoles.IsSink(node);
            var acc = statsByNode[node.Id];
            var readers = inboundEdges.Select(edge => channelByEdge[edge.Id].Reader).ToArray();

            // With one instance the join loop runs the node inline and order is trivially preserved. With
            // N, the loop becomes a dispatcher: instances finish out of order, so results pass through a
            // single emitter that releases them in cycle order (decision: strict order at sinks, always).
            var parallel = instances.Count > 1
                ? StartParallelInstances(node, instances)
                : null;

            async Task ProcessAsync(long cycleId, NodeExecutionResult? voidResult, NodeExecutionInputs? inputs)
            {
                if (parallel is null)
                {
                    if (voidResult is { } skipped)
                    {
                        Interlocked.Add(ref droppedFrames,
                            await RouteAsync(node, skipped, NodeExecutionInputs.Empty, cycleId));
                    }
                    else
                    {
                        var (result, _, _) = await RunOnceAsync(node, instances[0], inputs!);
                        Interlocked.Add(ref droppedFrames, await RouteAsync(node, result, inputs!, cycleId));
                        if (arenaActive)
                        {
                            DataPlaneRouter.ReleaseArenaInputs(inputs!, dataPlane!);
                        }
                    }

                    NoteLeafCompleted(node.Id, cycleId);
                    return;
                }

                // A void cycle skips the instances entirely but must still reach the emitter, or the
                // cycle sequence it reorders against would have a hole in it.
                if (voidResult is { } skippedResult)
                {
                    await parallel.Done.Writer.WriteAsync((cycleId, skippedResult, null), runToken);
                }
                else
                {
                    await parallel.Work.Writer.WriteAsync((cycleId, inputs!), runToken);
                }
            }

            try
            {
                while (true)
                {
                    // Read by hand rather than await-foreach so the wait for an input is timed on its own:
                    // a starved stage and a busy one look identical from throughput alone.
                    var readStart = Stopwatch.GetTimestamp();
                    var arrived = new StageMessage?[readers.Length];
                    var upstreamFinished = false;

                    for (var i = 0; i < readers.Length; i++)
                    {
                        while (true)
                        {
                            if (!await readers[i].WaitToReadAsync(runToken))
                            {
                                upstreamFinished = true;
                                break;
                            }

                            if (readers[i].TryRead(out var message))
                            {
                                arrived[i] = message;
                                break;
                            }
                        }

                        if (upstreamFinished)
                        {
                            break;
                        }
                    }

                    acc.ReadBlockedTicks += Stopwatch.GetTimestamp() - readStart;
                    if (upstreamFinished)
                    {
                        break;
                    }

                    // All inbound edges advance in lockstep, so a mismatch here means the void-marker
                    // invariant was broken upstream — fail loudly rather than pair the wrong frames.
                    var cycleId = arrived[0]!.Value.CycleId;
                    for (var i = 1; i < arrived.Length; i++)
                    {
                        if (arrived[i]!.Value.CycleId != cycleId)
                        {
                            FailRun($"Node '{node.Id}' joined inputs from different cycles "
                                  + $"({cycleId} vs {arrived[i]!.Value.CycleId}) — edge messages are out of step.");
                            return;
                        }
                    }

                    var context = new NodeExecutionContext
                    {
                        RunId = runId,
                        CycleIndex = (int)Math.Min(int.MaxValue, cycleId),
                        CycleStartedAt = DateTime.UtcNow
                    };

                    var values = new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase);
                    for (var i = 0; i < arrived.Length; i++)
                    {
                        if (arrived[i]!.Value.Value is { } value)
                        {
                            values[inboundEdges[i].To.Port] = value;
                        }
                    }

                    if (isSink)
                    {
                        ReportSinkCycle(cycleId, values.Count > 0);
                    }

                    // Nothing reached this node this cycle. The serial executor would run it against an
                    // empty bus and get NoOutput, so skip the call and pass the void on — same result,
                    // without paying for a node that has nothing to do.
                    if (values.Count == 0)
                    {
                        await ProcessAsync(cycleId, NodeExecutionResult.NoOutput, null);
                        continue;
                    }

                    await ProcessAsync(cycleId, null, new NodeExecutionInputs(values, context));
                }
            }
            finally
            {
                // Drain the instances before declaring the stage finished, so every in-flight frame is
                // routed and every leaf report lands before downstream sees the edge complete.
                if (parallel is not null)
                {
                    parallel.Work.Writer.TryComplete();
                    try { await Task.WhenAll(parallel.Executors); } catch { /* surfaced via FailRun */ }
                    parallel.Done.Writer.TryComplete();
                    try { await parallel.Emitter; } catch { /* surfaced via FailRun */ }
                }

                CompleteOutputs(node);
            }
        }

        // N instances of one node: a work queue they pull from, and a single emitter that puts their
        // results back into cycle order before routing. The emitter is also the only writer to this node's
        // outgoing edges, which keeps the single-writer channel contract intact.
        ParallelStage StartParallelInstances(PipelineNodeDefinition node, IReadOnlyList<INodeRunner> instances)
        {
            var work = Channel.CreateBounded<(long CycleId, NodeExecutionInputs Inputs)>(
                new BoundedChannelOptions(instances.Count)
                {
                    SingleReader = false, SingleWriter = true, FullMode = BoundedChannelFullMode.Wait
                });

            var done = Channel.CreateBounded<(long CycleId, NodeExecutionResult Result, NodeExecutionInputs? Inputs)>(
                new BoundedChannelOptions(instances.Count + 1)
                {
                    SingleReader = true, SingleWriter = false, FullMode = BoundedChannelFullMode.Wait
                });

            var executors = instances.Select(instance => Task.Run(async () =>
            {
                await foreach (var item in work.Reader.ReadAllAsync(runToken))
                {
                    var (result, _, _) = await RunOnceAsync(node, instance, item.Inputs);
                    await done.Writer.WriteAsync((item.CycleId, result, item.Inputs), runToken);
                }
            }, CancellationToken.None)).ToList();

            var emitter = Task.Run(async () =>
            {
                // At most instances.Count - 1 results can be waiting for an earlier one, so this buffer is
                // bounded by the parallelism, not by the run length.
                var pending = new Dictionary<long, (NodeExecutionResult Result, NodeExecutionInputs? Inputs)>();
                var nextToEmit = 0L;

                await foreach (var item in done.Reader.ReadAllAsync(runToken))
                {
                    pending[item.CycleId] = (item.Result, item.Inputs);

                    while (pending.Remove(nextToEmit, out var ready))
                    {
                        Interlocked.Add(ref droppedFrames, await RouteAsync(
                            node, ready.Result, ready.Inputs ?? NodeExecutionInputs.Empty, nextToEmit));

                        if (arenaActive && ready.Inputs is { } consumed)
                        {
                            DataPlaneRouter.ReleaseArenaInputs(consumed, dataPlane!);
                        }

                        NoteLeafCompleted(node.Id, nextToEmit);
                        nextToEmit++;
                    }
                }
            }, CancellationToken.None);

            return new ParallelStage(work, done, executors, emitter);
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
                var instances = instancesByNode[node.Id];
                var inbound = inboundByNode[node.Id].ToList();
                stages.Add(Task.Run(async () =>
                {
                    try
                    {
                        if (inbound.Count == 0)
                        {
                            await RunSourceStageAsync(node, instances[0]);
                        }
                        else
                        {
                            await RunConsumerStageAsync(node, instances, inbound);
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
                Worker = workerMetricsByNode.GetValueOrDefault(kvp.Key),
                Source = kvp.Value.Source?.ToProfile(),
                Stage = new StageProfile
                {
                    BusyMicros = TicksToMicros(kvp.Value.TotalDurationTicks),
                    RouteMicros = TicksToMicros(kvp.Value.RouteTicks),
                    WriteBlockedMicros = TicksToMicros(kvp.Value.WriteBlockedTicks),
                    ReadBlockedMicros = TicksToMicros(kvp.Value.ReadBlockedTicks)
                }
            },
            StringComparer.OrdinalIgnoreCase);

        // A cleanly, fully-consumed run has nothing to resume — drop its persisted checkpoint.
        if (sourceCompleted && runFailure is null && checkpointStore is not null)
        {
            try { await checkpointStore.ClearAsync(cancellationToken); }
            catch { /* best effort */ }
        }

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
        foreach (var node in executionOrder)
        {
            // Two edges into one port would race: whichever arrived first would win, and the join would
            // then be one message out of step on that edge forever. The serial port bus silently
            // overwrites instead; neither is a semantics worth guessing at.
            var duplicatePort = definition.Edges
                .Where(e => StringComparer.OrdinalIgnoreCase.Equals(e.To.NodeId, node.Id))
                .GroupBy(e => e.To.Port, StringComparer.OrdinalIgnoreCase)
                .FirstOrDefault(g => g.Count() > 1);
            if (duplicatePort is not null)
            {
                return $"Node '{node.Id}' has {duplicatePort.Count()} edges into the single input port "
                     + $"'{duplicatePort.Key}'. Pipelined mode needs one producer per input port so a join "
                     + "stays in step. Run this pipeline in serial mode.";
            }

            // Replication is opt-in at the module, because only the module author knows whether it keeps
            // state across frames — the engine cannot see that, and N instances of a stateful module means
            // N silently diverging states. Refuse rather than clamp, so the mismatch is visible.
            var requested = ResolveParallelism(node, catalog);
            var allowed = ResolveMaxParallelism(node, catalog);
            if (requested > allowed)
            {
                return $"Node '{node.Id}' asks for parallelism {requested} but its module allows at most "
                     + $"{allowed}. A module opts into replication with \"maxParallelism\" in module.json, "
                     + "which asserts it holds no state across frames.";
            }

            if (ResolveActivationMode(node, catalog) == NodeActivationMode.OnDemand)
            {
                return $"Node '{node.Id}' is on-demand. Lazy activation is serial-only for now; in pipelined "
                     + "mode every stage is warmed before the run. Run this pipeline in serial mode.";
            }

            // A value node has no inputs, so it has no queue to read and nothing to pace it — as a stage it
            // would emit for a cycle that may never come. Feeding constants into a pipelined graph needs a
            // decision (emit once and hold, or once per source cycle) that is not worth guessing at.
            if (string.Equals(node.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase)
                && node.PrimitiveType is "value" or "select")
            {
                return $"Node '{node.Id}' is a '{node.PrimitiveType}' primitive. Value resolution is "
                     + "serial-only for now. Run this pipeline in serial mode.";
            }

            // A `loop` is a whole-graph marker that carries the run's pause state — a property of the serial
            // cycle. The pipelined executor runs every stage continuously and has no pause gate yet, so it
            // refuses a loop rather than running it with pause silently doing nothing.
            if (string.Equals(node.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase)
                && node.PrimitiveType is "loop")
            {
                return $"Node '{node.Id}' is a 'loop' primitive. Pause is serial-only for now. "
                     + "Run this pipeline in serial mode.";
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

    /// <summary>
    /// How many instances of a node to run: the node's request, defaulting to 1. The module's declared
    /// ceiling is enforced separately (see <see cref="DescribeUnsupported"/>) so the failure names both
    /// numbers instead of silently clamping — a pipeline that asked for 8 and quietly got 1 would look
    /// like the feature simply did not work.
    /// </summary>
    private static int ResolveParallelism(
        PipelineNodeDefinition node,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog)
    {
        _ = catalog;
        return node.Parallelism is { } requested && requested > 1 ? requested : 1;
    }

    private static int ResolveMaxParallelism(
        PipelineNodeDefinition node,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog) =>
        node.ModuleId is { } id
        && catalog is not null
        && catalog.TryGetValue(id, out var entry)
        && entry.Manifest.MaxParallelism is { } max
            ? Math.Max(1, max)
            : 1;

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
        public long RouteTicks;
        public long WriteBlockedTicks;
        public long ReadBlockedTicks;
        public WorkerMetricsSnapshot? Worker;
        public SourceAcquisitionAccumulator? Source;
    }
}
