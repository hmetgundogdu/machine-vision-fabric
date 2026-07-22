using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Runtime;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;

namespace Mvf.Engine.Tests;

/// <summary>
/// On-demand loading (L.3): a resident node is preloaded before the first cycle, but an <c>on-demand</c>
/// node (a short helper) is <b>not activated until a frame actually reaches it</b>, and is skipped on the
/// cycles it is idle. So a gated helper costs nothing — no process spawn, no warmup — until it is used.
/// See docs/module-lifecycle-design.md.
/// </summary>
public sealed class OnDemandActivationTests
{
    [Fact]
    public async Task OnDemand_NodeThatNeverReceivesAFrame_IsNeverActivated()
    {
        // source1 (resident) drives the loop; lazy1 (on-demand) has an input but no edge feeds it.
        var source = new RecordingSourceRunner("source1", frames: 1);
        var lazy = new RecordingRunner("lazy1");
        var activator = new FakeActivator(("source1", source), ("lazy1", lazy));

        var report = await ExecuteAsync(activator, UnfedOnDemand());

        Assert.True(report.Succeeded);
        Assert.True(source.Activated);       // resident: preloaded
        Assert.False(lazy.Activated);        // on-demand + never used: never activated (no spawn/warmup)
        Assert.Equal(0, report.NodeStats["lazy1"].TotalCycles);
        Assert.Equal(NodeActivationMode.OnDemand, report.NodeStats["lazy1"].ActivationMode);
    }

    [Fact]
    public async Task OnDemand_NodeThatReceivesAFrame_ActivatesLazilyAndRuns()
    {
        var source = new RecordingSourceRunner("source1", frames: 1);
        var lazy = new RecordingRunner("lazy1");
        var activator = new FakeActivator(("source1", source), ("lazy1", lazy));

        var report = await ExecuteAsync(activator, SourceToOnDemand());

        Assert.True(report.Succeeded);
        Assert.True(lazy.Activated);         // activated lazily on first use
        Assert.Equal(1, lazy.FramesSeen);    // and it actually ran with the frame
        Assert.Equal(1, report.NodeStats["lazy1"].TotalCycles);
        Assert.Equal(NodeActivationMode.OnDemand, report.NodeStats["lazy1"].ActivationMode);
    }

    private static async Task<PipelineExecutionReport> ExecuteAsync(FakeActivator activator, PipelineDefinition definition)
    {
        var repo = FindRepoRoot();
        var executor = new PipelineGraphExecutor(activator, dataPlane: null, new ModuleCatalog());
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = Path.Combine(repo, "modules"),
            MaxCycles = 1
        };

        return await executor.ExecuteAsync(definition, options, CancellationToken.None);
    }

    private static PipelineDefinition SourceToOnDemand() => new()
    {
        Name = "source-to-ondemand",
        Nodes = [SourceNode("source1"), OnDemandNode("lazy1")],
        Edges =
        [
            new PipelineEdgeDefinition
            {
                Id = "e1", Kind = "data",
                From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                To = new PipelinePortReference { NodeId = "lazy1", Port = "frame" }
            }
        ]
    };

    // lazy1 has an input port but nothing routes to it — it stays idle every cycle.
    private static PipelineDefinition UnfedOnDemand() => new()
    {
        Name = "unfed-ondemand",
        Nodes = [SourceNode("source1"), OnDemandNode("lazy1")],
        Edges = []
    };

    private static PipelineNodeDefinition SourceNode(string id) => new()
    {
        Id = id, Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
        ActivationMode = null, // → module default (resident)
        Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", AllowMultipleEdges = true }]
    };

    private static PipelineNodeDefinition OnDemandNode(string id) => new()
    {
        Id = id, Kind = "integration-module", Category = "compute", ModuleId = "mvf.realworld-dark-frame-filter",
        ActivationMode = "on-demand",
        Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
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

    private sealed class RecordingSourceRunner(string nodeId, int frames) : INodeRunner
    {
        private int _index;
        public bool Activated { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) { Activated = true; return Task.CompletedTask; }

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(_index >= frames
                ? NodeExecutionResult.NoOutput
                : NodeExecutionResult.Single("frame", PortValue.FromFrame(
                    (IFrameEnvelope)new BinaryFrameEnvelope("cam1", ++_index, "f.bmp", [1, 2, 3], "image/bmp"))));

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class RecordingRunner(string nodeId) : INodeRunner
    {
        public bool Activated { get; private set; }
        public int FramesSeen { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) { Activated = true; return Task.CompletedTask; }

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
