using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Cross-process observability (M3): a supervised worker restart is transparent to the graph — the retry
/// just succeeds — so without explicit accounting a run that lost and replaced a child looks identical to
/// one that never did. These tests pin that the counters exist at the adapter, survive the trip through
/// the node runner, and land in the execution report. The Python ones require python3 on PATH.
/// </summary>
public sealed class WorkerObservabilityTests
{
    [Fact]
    public async Task Execute_WithWorkerBackedNode_CarriesWorkerMetricsIntoTheReport()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        // Absorbs a "crash" on its second frame, exactly as SupervisedWorker does for a real child.
        var worker = new StubWorkerRunner("class1", "py.stub-classifier", restartOnRequest: 2);
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", worker));

        var observed = new List<NodeExecutionEvent>();
        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(
            BuildSourceToWorker(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".",
                IntegrationsRoot = ".",
                OnNodeExecuted = e => observed.Add(e)
            },
            CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(1, report.WorkerRestarts);

        var stats = report.NodeStats["class1"];
        Assert.NotNull(stats.Worker);
        Assert.Equal("py.stub-classifier", stats.Worker!.ModuleId);
        Assert.Equal(3L, stats.Worker.Requests);
        Assert.Equal(1, stats.Worker.Restarts);
        Assert.NotNull(stats.Worker.LastRestartUtc);
        Assert.True(stats.Worker.AverageRequestMs > 0);

        // An in-process node has no worker behind it and must not invent one.
        Assert.Null(report.NodeStats["source1"].Worker);

        // Live signal: the count moves on the cycle the crash was absorbed, not only at the end.
        var workerEvents = observed.Where(e => e.NodeId == "class1").Select(e => e.WorkerRestarts).ToList();
        Assert.Equal([0, 1, 1], workerEvents);
    }

    [Fact]
    public async Task WorkerMetrics_AfterCrash_RecordRestartWithReasonAndLatency()
    {
        var repo = FindRepoRoot();
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var launch = CounterLaunch(repo, arena);
        await using var supervised = await SupervisedWorker.StartAsync(
            token => StdioWorkerProcess.StartAsync(launch, token), arena, cts.Token);

        var classifier = new WorkerFrameClassifier(supervised, arena);
        var metrics = (IWorkerMetricsSource)classifier;

        await classifier.ClassifyAsync(Frame(1), cts.Token);
        await classifier.ClassifyAsync(Frame(2), cts.Token);

        var before = metrics.GetWorkerMetrics();
        Assert.NotNull(before);
        Assert.Equal(2L, before!.Requests);
        Assert.Equal(0, before.Restarts);
        Assert.Null(before.LastRestartUtc);
        Assert.True(before.AverageRequestMs > 0, "a real stdio round-trip must register some latency");

        // Per-node resource usage: a live child process reports a real working set; CPU% is 0 on the first
        // sample (no prior reading to difference against) but never negative.
        Assert.True(before.WorkingSetBytes > 0, "a live worker child must report a non-zero working set");
        Assert.True(before.CpuPercent >= 0);

        await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);
        supervised.KillCurrentWorkerForTest();

        // Transparent recovery: the caller sees a normal result, the counters see a restart.
        var third = await classifier.ClassifyAsync(Frame(3), cts.Token);
        Assert.Equal(3d, third.Measurement);

        var after = metrics.GetWorkerMetrics();
        Assert.NotNull(after);
        Assert.Equal(3L, after!.Requests);              // checkpoint RPCs are not counted as work
        Assert.Equal(0L, after.FailedRequests);         // the crash never surfaced as a failed request
        Assert.Equal(1, after.Restarts);
        Assert.Equal(0, after.WarmRestarts);            // no pool → the restart paid a cold spawn
        Assert.NotNull(after.LastRestartUtc);
        Assert.False(string.IsNullOrWhiteSpace(after.LastRestartReason));
        Assert.True(after.TotalRequestMicros > before.TotalRequestMicros);
    }

    [Fact]
    public async Task WorkerMetrics_RestartFromWarmPool_IsCountedAsWarm()
    {
        var repo = FindRepoRoot();
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var launch = CounterLaunch(repo, arena);
        Task<StdioWorkerProcess> Spawn(CancellationToken t) => StdioWorkerProcess.StartAsync(launch, t);

        var pool = await WarmWorkerPool.StartAsync(Spawn, targetSpares: 1, cts.Token);
        await using var supervised = await SupervisedWorker.StartAsync(Spawn, arena, cts.Token, pool);
        var classifier = new WorkerFrameClassifier(supervised, arena);

        await classifier.ClassifyAsync(Frame(1), cts.Token);
        await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);

        // The initial worker came from the pool; wait for the background replenish so the restart is
        // deterministically served warm rather than racing the cold-spawn fallback.
        while (pool.ReadyCount == 0)
        {
            await Task.Delay(25, cts.Token);
        }

        supervised.KillCurrentWorkerForTest();
        var second = await classifier.ClassifyAsync(Frame(2), cts.Token);
        Assert.Equal(2d, second.Measurement);   // state restored on the spare

        var metrics = ((IWorkerMetricsSource)classifier).GetWorkerMetrics();
        Assert.NotNull(metrics);
        Assert.Equal(1, metrics!.Restarts);
        Assert.Equal(1, metrics.WarmRestarts);  // recovery skipped the cold start (L.4)
    }

    // ---- helpers ----

    private static WorkerLaunchInfo CounterLaunch(string repo, SharedMemoryArena arena)
    {
        var moduleDir = Path.Combine(repo, "modules", "py-frame-counter");
        return new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);
    }

    private static IFrameEnvelope Frame(int seq) =>
        new BinaryFrameEnvelope("cam1", seq, $"f{seq}.bmp", [0], "image/bmp");

    private static PipelineDefinition BuildSourceToWorker() => new()
    {
        Name = "source-to-worker",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "runtime-builtin", Category = "source",
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            },
            new PipelineNodeDefinition
            {
                Id = "class1", Kind = "integration-module", Category = "classify", ModuleId = "py.stub-classifier",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition
            {
                Id = "e1", Kind = "data",
                From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                To = new PipelinePortReference { NodeId = "class1", Port = "frame" }
            }
        ]
    };

    private static string FindRepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "CLAUDE.md"))
                && Directory.Exists(Path.Combine(dir.FullName, "modules")))
            {
                return dir.FullName;
            }
            dir = dir.Parent;
        }
        throw new InvalidOperationException("Repository root (with CLAUDE.md + modules/) not found.");
    }

    // ---- fakes ----

    private sealed class FakeActivator(params (string NodeId, INodeRunner Runner)[] runners) : IPipelineNodeActivator
    {
        private readonly Dictionary<string, INodeRunner> _runners =
            runners.ToDictionary(r => r.NodeId, r => r.Runner, StringComparer.OrdinalIgnoreCase);

        public async Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node, PipelineExecutionOptions options, CancellationToken cancellationToken)
        {
            var runner = _runners[node.Id];
            await runner.ActivateAsync(cancellationToken);
            return runner;
        }
    }

    private sealed class FakeSourceRunner(string nodeId, IReadOnlyList<IFrameEnvelope> frames) : INodeRunner
    {
        private int _index;
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(_index >= frames.Count
                ? NodeExecutionResult.NoOutput
                : NodeExecutionResult.Single("frame", PortValue.FromFrame(frames[_index++])));

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>
    /// A node runner standing in for a worker-backed one: it consumes frames and reports cross-process
    /// counters, absorbing one restart on a chosen request — the observable shape of a real recovery.
    /// </summary>
    private sealed class StubWorkerRunner(string nodeId, string moduleId, int restartOnRequest)
        : INodeRunner, IWorkerMetricsSource
    {
        private long _requests;
        private int _restarts;
        private DateTime? _lastRestartUtc;

        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is null)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _requests++;
            if (_requests == restartOnRequest)
            {
                _restarts++;
                _lastRestartUtc = DateTime.UtcNow;
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public WorkerMetricsSnapshot GetWorkerMetrics() => new()
        {
            ModuleId = moduleId,
            Requests = _requests,
            TotalRequestMicros = _requests * 1_500,
            MaxRequestMicros = _restarts > 0 ? 90_000 : 2_000,
            Restarts = _restarts,
            WarmRestarts = _restarts,
            LastRestartUtc = _lastRestartUtc,
            LastRestartReason = _restarts > 0 ? "worker exited before responding" : null
        };

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
