using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// Backpressure at the arena publish boundary (M3): when a producer cannot place a frame in the shared
/// data plane, the executor applies a policy instead of silently corrupting or throwing an undiagnosed
/// node error. <b>Stall</b> (lossless) fails the run with an actionable message; <b>Drop</b> (lossy)
/// skips the frame for out-of-process consumers, counts it, and keeps the source running. A frame that
/// can never fit a slot is a permanent sizing error and stops the run under any policy. Verified with a
/// data plane that reports the arena as full — no Python needed.
/// </summary>
public sealed class PipelineGraphExecutorBackpressureTests
{
    [Fact]
    public async Task Drop_ArenaFull_SkipsWorkerFramesCountsThemAndKeepsRunning()
    {
        var repo = FindRepoRoot();
        var dataPlane = new FullDataPlane(slotSize: int.MaxValue); // full, but a slot could in principle hold the frame

        var frames = new IFrameEnvelope[]
        {
            new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp"),
            new BinaryFrameEnvelope("cam1", 2, "f2.bmp", [4, 5, 6], "image/bmp")
        };
        var worker = new FrameRecordingRunner("class1");
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", worker));

        var report = await ExecuteAsync(activator, dataPlane, repo, BackpressurePolicy.Drop);

        Assert.True(report.Succeeded);
        Assert.Equal(2, report.TotalCycles);      // the source kept producing both frames
        Assert.Equal(2, report.DroppedFrames);    // both were dropped for the worker (arena full)
        Assert.Equal(0, worker.FramesSeen);       // the out-of-process consumer got nothing
    }

    [Fact]
    public async Task Stall_ArenaFull_FailsRunWithActionableMessage()
    {
        var repo = FindRepoRoot();
        var dataPlane = new FullDataPlane(slotSize: int.MaxValue);

        var frames = new IFrameEnvelope[] { new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp") };
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", new FrameRecordingRunner("class1")));

        // Stall is the default policy — assert it is also the default.
        var report = await ExecuteAsync(activator, dataPlane, repo, BackpressurePolicy.Stall);

        Assert.False(report.Succeeded);
        Assert.Equal(0, report.DroppedFrames);                 // lossless never drops
        Assert.NotNull(report.ErrorMessage);
        Assert.Contains("full", report.ErrorMessage!, StringComparison.OrdinalIgnoreCase);
        Assert.Contains("drop", report.ErrorMessage!, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task PayloadTooLarge_FailsRunEvenUnderDropPolicy()
    {
        var repo = FindRepoRoot();
        // Slot too small for the frame → the failure is permanent (sizing), not transient backpressure.
        var dataPlane = new FullDataPlane(slotSize: 1);

        var frames = new IFrameEnvelope[] { new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3, 4], "image/bmp") };
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", new FrameRecordingRunner("class1")));

        var report = await ExecuteAsync(activator, dataPlane, repo, BackpressurePolicy.Drop);

        Assert.False(report.Succeeded);                        // dropping every frame forever is not useful
        Assert.Equal(0, report.DroppedFrames);
        Assert.NotNull(report.ErrorMessage);
        Assert.Contains("slot", report.ErrorMessage!, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task PerNodeOverride_DropBeatsRunLevelStall_DropsAndSucceeds()
    {
        // Run default is Stall (lossless), but the source declares backpressure=drop (e.g. live camera).
        var repo = FindRepoRoot();
        var dataPlane = new FullDataPlane(slotSize: int.MaxValue);
        var frames = new IFrameEnvelope[]
        {
            new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp"),
            new BinaryFrameEnvelope("cam1", 2, "f2.bmp", [4, 5, 6], "image/bmp")
        };
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", new FrameRecordingRunner("class1")));

        var report = await ExecuteAsync(activator, dataPlane, repo, BackpressurePolicy.Stall, sourceBackpressure: "drop");

        Assert.True(report.Succeeded);          // the per-source override won over the lossless run default
        Assert.Equal(2, report.DroppedFrames);
    }

    [Fact]
    public async Task PerNodeOverride_StallBeatsRunLevelDrop_FailsRun()
    {
        // Run default is Drop (lossy), but the source declares backpressure=stall (e.g. folder replay).
        var repo = FindRepoRoot();
        var dataPlane = new FullDataPlane(slotSize: int.MaxValue);
        var frames = new IFrameEnvelope[] { new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp") };
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("class1", new FrameRecordingRunner("class1")));

        var report = await ExecuteAsync(activator, dataPlane, repo, BackpressurePolicy.Drop, sourceBackpressure: "stall");

        Assert.False(report.Succeeded);         // the per-source override won over the lossy run default
        Assert.Equal(0, report.DroppedFrames);
        Assert.NotNull(report.ErrorMessage);
    }

    [Fact]
    public void Validator_RejectsUnknownBackpressure()
    {
        var validator = new PipelineDefinitionValidator();
        var definition = new PipelineDefinition
        {
            Name = "bad",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "source1", Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
                    Backpressure = "drop-ish",
                    Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
                }
            ]
        };

        var result = validator.Validate(definition);

        Assert.False(result.IsValid);
        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-backpressure");
    }

    private static async Task<PipelineExecutionReport> ExecuteAsync(
        FakeActivator activator, IDataPlane dataPlane, string repo, BackpressurePolicy policy, string? sourceBackpressure = null)
    {
        var executor = new PipelineGraphExecutor(activator, dataPlane, new ModuleCatalog());
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = Path.Combine(repo, "modules"),
            MaxCycles = 0,
            BackpressurePolicy = policy
        };

        return await executor.ExecuteAsync(SourceToWorker(sourceBackpressure), options, CancellationToken.None);
    }

    private static PipelineDefinition SourceToWorker(string? sourceBackpressure = null) => new()
    {
        Name = "source-to-worker",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
                Backpressure = sourceBackpressure,
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", AllowMultipleEdges = true }]
            },
            new PipelineNodeDefinition
            {
                Id = "class1", Kind = "integration-module", Category = "classify", ModuleId = "py.brightness-classifier",
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
        while (dir is not null && !Directory.Exists(Path.Combine(dir.FullName, "modules")))
        {
            dir = dir.Parent;
        }

        return dir?.FullName ?? throw new InvalidOperationException("Could not locate repo root (with modules/).");
    }

    // ---- fakes ----

    /// <summary>A data plane whose arena is always full: every publish/reserve fails.</summary>
    private sealed class FullDataPlane(int slotSize) : IDataPlane
    {
        public string BackingPath => "/tmp/full-arena";
        public int SlotSize { get; } = slotSize;

        public bool TryPublish(in PayloadDescriptor descriptor, ReadOnlySpan<byte> payload, int referenceCount, out ArenaHandle handle)
        {
            handle = default;
            return false;
        }

        public bool TryReserve(out ArenaHandle handle)
        {
            handle = default;
            return false;
        }

        public bool TryReadDescriptor(ArenaHandle handle, out PayloadDescriptor descriptor)
        {
            descriptor = default;
            return false;
        }

        public Stream OpenRead(ArenaHandle handle) => new MemoryStream();
        public void AddRef(ArenaHandle handle, int count) { }
        public void Release(ArenaHandle handle) { }
    }

    private sealed class FakeActivator(params (string NodeId, INodeRunner Runner)[] runners) : IPipelineNodeActivator
    {
        private readonly Dictionary<string, INodeRunner> _runners =
            runners.ToDictionary(r => r.NodeId, r => r.Runner, StringComparer.OrdinalIgnoreCase);

        public async Task<INodeRunner> ActivateAsync(PipelineNodeDefinition node, PipelineExecutionOptions options, CancellationToken cancellationToken)
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

    /// <summary>Counts how many frames actually reached this (worker) consumer.</summary>
    private sealed class FrameRecordingRunner(string nodeId) : INodeRunner
    {
        public int FramesSeen { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is not null)
            {
                FramesSeen++;
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
