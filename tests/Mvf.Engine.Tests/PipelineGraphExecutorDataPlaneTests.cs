using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;

namespace Mvf.Engine.Tests;

/// <summary>
/// The graph-aware data plane (M2 Slice B.2): a frame that fans out to several out-of-process workers
/// is published into the arena <b>once</b> — refcount = worker edges — and each consumer releases after
/// it runs, so the single shared copy is reclaimed once the last worker has read it. Transport is
/// chosen per edge from the static graph. Verified with a recording data plane (no Python needed).
/// </summary>
public sealed class PipelineGraphExecutorDataPlaneTests
{
    [Fact]
    public async Task Execute_FrameFanoutToTwoWorkers_PublishesOnceSharedWithRefcountTwo()
    {
        var repo = FindRepoRoot();
        var dataPlane = new RecordingDataPlane();

        var frame = (IFrameEnvelope)new Mvf.Abstractions.Frames.BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp");
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", [frame])),
            ("class1", new FrameConsumingRunner("class1")),
            ("class2", new FrameConsumingRunner("class2")));

        // ModuleCatalog over the real modules/ tree: py.brightness-classifier is a python (worker) module.
        var executor = new PipelineGraphExecutor(activator, dataPlane, new ModuleCatalog());
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = Path.Combine(repo, "modules"),
            MaxCycles = 1
        };

        var report = await executor.ExecuteAsync(BuildFanoutToTwoWorkers(), options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(1, dataPlane.PublishCount);       // one copy, not one per worker
        Assert.Equal(2, dataPlane.LastReferenceCount); // refcount = number of worker edges
        Assert.Equal(2, dataPlane.ReleaseCount);       // each worker releases after it runs
        Assert.True(dataPlane.AllReclaimed);           // slot returned to the pool at zero
    }

    [Fact]
    public async Task Execute_FrameToOneWorkerAndOneInProcess_PublishesForTheWorkerOnly()
    {
        var repo = FindRepoRoot();
        var dataPlane = new RecordingDataPlane();

        var frame = (IFrameEnvelope)new Mvf.Abstractions.Frames.BinaryFrameEnvelope("cam1", 1, "f1.bmp", [7, 7], "image/bmp");
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", [frame])),
            ("class1", new FrameConsumingRunner("class1")),   // worker
            ("sink1", new FrameConsumingRunner("sink1")));     // in-process

        var executor = new PipelineGraphExecutor(activator, dataPlane, new ModuleCatalog());
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = Path.Combine(repo, "modules"),
            MaxCycles = 1
        };

        var report = await executor.ExecuteAsync(BuildFanoutToWorkerAndSink(), options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(1, dataPlane.PublishCount);       // published once, for the single worker edge
        Assert.Equal(1, dataPlane.LastReferenceCount); // the in-process consumer keeps the heap frame
        Assert.Equal(1, dataPlane.ReleaseCount);
        Assert.True(dataPlane.AllReclaimed);
    }

    private static PipelineDefinition BuildFanoutToTwoWorkers() => new()
    {
        Name = "fanout",
        Nodes =
        [
            SourceNode("source1"),
            WorkerNode("class1"),
            WorkerNode("class2")
        ],
        Edges =
        [
            DataEdge("e1", "source1", "frame", "class1", "frame"),
            DataEdge("e2", "source1", "frame", "class2", "frame")
        ]
    };

    private static PipelineDefinition BuildFanoutToWorkerAndSink() => new()
    {
        Name = "worker-and-sink",
        Nodes =
        [
            SourceNode("source1"),
            WorkerNode("class1"),
            new PipelineNodeDefinition
            {
                Id = "sink1", Kind = "integration-module", Category = "output", ModuleId = "mvf.dataset-writer",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            DataEdge("e1", "source1", "frame", "class1", "frame"),
            DataEdge("e2", "source1", "frame", "sink1", "frame")
        ]
    };

    private static PipelineNodeDefinition SourceNode(string id) => new()
    {
        Id = id, Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
        Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", AllowMultipleEdges = true }]
    };

    private static PipelineNodeDefinition WorkerNode(string id) => new()
    {
        Id = id, Kind = "integration-module", Category = "classify", ModuleId = "py.brightness-classifier",
        Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
    };

    private static PipelineEdgeDefinition DataEdge(string id, string fromNode, string fromPort, string toNode, string toPort) => new()
    {
        Id = id, Kind = "data",
        From = new PipelinePortReference { NodeId = fromNode, Port = fromPort },
        To = new PipelinePortReference { NodeId = toNode, Port = toPort }
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

    private sealed class RecordingDataPlane : IDataPlane
    {
        private long _nextOffset;
        private readonly Dictionary<long, int> _refByOffset = [];
        private readonly Dictionary<long, PayloadDescriptor> _descriptorByOffset = [];

        public int PublishCount { get; private set; }
        public int ReleaseCount { get; private set; }
        public int LastReferenceCount { get; private set; }
        public bool AllReclaimed => _refByOffset.Values.All(r => r <= 0);

        public string BackingPath => "/tmp/recording-arena";
        public int SlotSize => int.MaxValue;

        public bool TryPublish(in PayloadDescriptor descriptor, ReadOnlySpan<byte> payload, int referenceCount, out ArenaHandle handle)
        {
            var offset = _nextOffset;
            _nextOffset += payload.Length + 1;
            handle = new ArenaHandle(offset, payload.Length);
            _refByOffset[offset] = referenceCount;
            _descriptorByOffset[offset] = descriptor;
            PublishCount++;
            LastReferenceCount = referenceCount;
            return true;
        }

        public bool TryReadDescriptor(ArenaHandle handle, out PayloadDescriptor descriptor) =>
            _descriptorByOffset.TryGetValue(handle.Offset, out descriptor);

        public Stream OpenRead(ArenaHandle handle) => new MemoryStream();

        public void Release(ArenaHandle handle)
        {
            ReleaseCount++;
            if (_refByOffset.TryGetValue(handle.Offset, out var count))
            {
                _refByOffset[handle.Offset] = count - 1;
            }
        }
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

    private sealed class FrameConsumingRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            _ = inputs.Get("frame"); // reads its input; the engine releases the handle after this returns
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
