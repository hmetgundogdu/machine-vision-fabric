using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Runtime;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// Module lifecycle contract made real & observed (L.1): the <c>activationMode</c> string is parsed and
/// validated (no longer decorative), a module's declared <c>lifecycle</c> supplies the default, a node
/// overrides it, and the executor measures each node's warmup (activation) duration and reports its
/// resolved loading profile. Generalizes past ML models — activation is any readiness work (model load,
/// device connect, init). See docs/module-lifecycle-design.md.
/// </summary>
public sealed class NodeLifecycleTests
{
    [Theory]
    [InlineData("resident", true, NodeActivationMode.Resident)]
    [InlineData("on-demand", true, NodeActivationMode.OnDemand)]
    [InlineData("ondemand", true, NodeActivationMode.OnDemand)]
    [InlineData("ON-DEMAND", true, NodeActivationMode.OnDemand)]
    [InlineData("resident-ish", false, NodeActivationMode.Resident)]
    [InlineData("", false, NodeActivationMode.Resident)]
    public void TryParse_AcceptsKnownProfilesRejectsUnknown(string value, bool expectedOk, NodeActivationMode expectedMode)
    {
        var ok = NodeActivationModes.TryParse(value, out var mode);
        Assert.Equal(expectedOk, ok);
        Assert.Equal(expectedMode, mode);
    }

    [Fact]
    public void Validator_RejectsUnknownActivationMode()
    {
        var validator = new PipelineDefinitionValidator();
        var definition = OneNode(activationMode: "warm-ish");

        var result = validator.Validate(definition);

        Assert.False(result.IsValid);
        Assert.Contains(result.Issues, i => i.Code == "pipeline.node.invalid-activation-mode");
    }

    [Fact]
    public void Validator_AcceptsKnownAndUnspecifiedActivationMode()
    {
        var validator = new PipelineDefinitionValidator();

        Assert.DoesNotContain(validator.Validate(OneNode(activationMode: "on-demand")).Issues,
            i => i.Code == "pipeline.node.invalid-activation-mode");
        Assert.DoesNotContain(validator.Validate(OneNode(activationMode: null)).Issues,
            i => i.Code == "pipeline.node.invalid-activation-mode");
    }

    [Fact]
    public async Task Executor_MeasuresWarmupAndResolvesLoadingProfile()
    {
        var repo = FindRepoRoot();

        // source1: no activationMode → inherits the module's declared lifecycle ("resident" on cognex).
        // helper1: explicit on-demand overrides. A ~40ms activation delay must show up as warmup.
        var activator = new FakeActivator(
            ("source1", new DelayingSourceRunner("source1", warmup: TimeSpan.FromMilliseconds(40))),
            ("helper1", new FrameConsumingRunner("helper1")));

        var executor = new PipelineGraphExecutor(activator, dataPlane: null, new ModuleCatalog());
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = Path.Combine(repo, "modules"),
            MaxCycles = 1
        };

        var report = await executor.ExecuteAsync(BuildSourceToHelper(), options, CancellationToken.None);

        Assert.True(report.Succeeded);
        // Module-declared default (cognex lifecycle = resident) resolved for the un-annotated source.
        Assert.Equal(NodeActivationMode.Resident, report.NodeStats["source1"].ActivationMode);
        // Per-node override wins over the module default.
        Assert.Equal(NodeActivationMode.OnDemand, report.NodeStats["helper1"].ActivationMode);
        // The slow activation is visible, not silent.
        Assert.True(report.NodeStats["source1"].WarmupMs >= 25,
            $"expected warmup to be measured, got {report.NodeStats["source1"].WarmupMs}ms");
    }

    private static PipelineDefinition BuildSourceToHelper() => new()
    {
        Name = "source-to-helper",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
                ActivationMode = null, // inherit module default
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", AllowMultipleEdges = true }]
            },
            new PipelineNodeDefinition
            {
                Id = "helper1", Kind = "integration-module", Category = "compute", ModuleId = "mvf.dark-frame-filter",
                ActivationMode = "on-demand", // override
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition
            {
                Id = "e1", Kind = "data",
                From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                To = new PipelinePortReference { NodeId = "helper1", Port = "frame" }
            }
        ]
    };

    private static PipelineDefinition OneNode(string? activationMode) => new()
    {
        Name = "one",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "n1", Kind = "integration-module", Category = "source", ModuleId = "mvf.realworld-cognex-camera",
                ActivationMode = activationMode,
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
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

    /// <summary>A source that takes measurable time to activate (mimics a model load / device connect).</summary>
    private sealed class DelayingSourceRunner(string nodeId, TimeSpan warmup) : INodeRunner
    {
        private bool _emitted;
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.Delay(warmup, cancellationToken);

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_emitted)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _emitted = true;
            var frame = (IFrameEnvelope)new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp");
            return Task.FromResult(NodeExecutionResult.Single("frame", PortValue.FromFrame(frame)));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class FrameConsumingRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            _ = inputs.Get("frame");
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
