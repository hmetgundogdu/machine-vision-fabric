using Mvf.Graph.Control;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;

namespace Mvf.Engine.Tests;

public sealed class PipelineGraphExecutorTests
{
    /// <summary>
    /// Verifies that a source → if → sink pipeline with gate=true
    /// correctly counts accepted cycles using a fake activator.
    /// </summary>
    [Fact]
    public async Task ExecuteAsync_SourceGateBranchSink_CountsAcceptedCycles()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope($"cam1", i, $"frame{i}.jpg", [(byte)i]))
            .ToArray();

        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: true)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", new FakeSinkRunner("sink1")));

        var executor = new PipelineGraphExecutor(activator);
        var definition = BuildTypicalDefinition();
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        };

        var report = await executor.ExecuteAsync(definition, options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(3, report.TotalCycles);
        Assert.Equal(3, report.AcceptedCycles);
    }

    [Fact]
    public async Task ExecuteAsync_GateClosed_ProducesZeroAcceptedCycles()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"frame{i}.jpg", [(byte)i]))
            .ToArray();

        var sink = new FakeSinkRunner("sink1");
        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: false)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", sink));

        var executor = new PipelineGraphExecutor(activator);
        var definition = BuildTypicalDefinition();
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        };

        var report = await executor.ExecuteAsync(definition, options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(3, report.TotalCycles);
        Assert.Empty(sink.ReceivedFrames);
    }

    [Fact]
    public async Task ExecuteAsync_RespectsMaxCycles()
    {
        var frames = Enumerable.Range(1, 100)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"frame{i}.jpg", [(byte)(i % 256)]))
            .ToArray();

        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: true)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", new FakeSinkRunner("sink1")));

        var executor = new PipelineGraphExecutor(activator);
        var definition = BuildTypicalDefinition();
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = ".",
            MaxCycles = 5
        };

        var report = await executor.ExecuteAsync(definition, options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(5, report.TotalCycles);
    }

    [Fact]
    public async Task ExecuteAsync_PopulatesNodeStats()
    {
        var frames = Enumerable.Range(1, 2)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"frame{i}.jpg", [(byte)i]))
            .ToArray();

        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: true)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", new FakeSinkRunner("sink1")));

        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(BuildTypicalDefinition(), new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        }, CancellationToken.None);

        Assert.NotEmpty(report.NodeStats);
        Assert.True(report.NodeStats.ContainsKey("source1"));
        // Source is called once per cycle plus one final call that returns NoOutput (exhaustion detection)
        Assert.Equal(3, report.NodeStats["source1"].TotalCycles);
    }

    /// <summary>
    /// These fakes run in well under a millisecond. Timing them in whole milliseconds truncated every
    /// cycle to zero, so a fast node reported as costing nothing — and could even come out cheaper than
    /// the worker RPC nested inside it. Node timing is measured in microseconds.
    /// </summary>
    [Fact]
    public async Task ExecuteAsync_TimesSubMillisecondNodes()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"frame{i}.jpg", [(byte)i]))
            .ToArray();

        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: true)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", new FakeSinkRunner("sink1")));

        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(BuildTypicalDefinition(), new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        }, CancellationToken.None);

        var sink = report.NodeStats["sink1"];
        Assert.True(sink.TotalCycles > 0);
        Assert.True(sink.TotalDurationMicros > 0, "a node that ran must not be reported as free");
        Assert.True(sink.AverageDurationMs > 0);
    }

    [Fact]
    public async Task ExecuteAsync_ContextPassedToNodes()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"frame{i}.jpg", [(byte)i]))
            .ToArray();

        var contextCapture = new List<NodeExecutionContext?>();
        var capturingSink = new ContextCapturingSinkRunner("sink1", contextCapture);

        var activator = new FakeNodeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("gate1", new FakeGateRunner("gate1", gateOpen: true)),
            ("branch1", new IfPrimitiveNodeRunnerWrapper("branch1")),
            ("sink1", capturingSink));

        var executor = new PipelineGraphExecutor(activator);
        await executor.ExecuteAsync(BuildTypicalDefinition(), new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        }, CancellationToken.None);

        Assert.Equal(3, contextCapture.Count);
        Assert.NotNull(contextCapture[0]);
        Assert.Equal(0, contextCapture[0]!.CycleIndex);
        Assert.Equal(1, contextCapture[1]!.CycleIndex);
        Assert.Equal(2, contextCapture[2]!.CycleIndex);
        // All cycles share the same RunId
        Assert.Equal(contextCapture[0]!.RunId, contextCapture[1]!.RunId);
    }

    private static PipelineDefinition BuildTypicalDefinition() =>
        new()
        {
            Name = "test-graph",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "source1", Kind = "integration-module", Category = "source",
                    Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "gate1", Kind = "integration-module", Category = "control",
                    Outputs = [new PipelinePortDefinition { Name = "productPresent", Channel = "control", DataType = "boolean-gate" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "branch1", Kind = "embedded-primitive", Category = "flow-control", PrimitiveType = "if",
                    Inputs =
                    [
                        new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" },
                        new PipelinePortDefinition { Name = "productPresent", Channel = "control", DataType = "boolean-gate" }
                    ],
                    Outputs = [new PipelinePortDefinition { Name = "acceptedFrame", Channel = "data", DataType = "frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "sink1", Kind = "runtime-builtin", Category = "output", BuiltinType = "dataset-writer",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "branch1", Port = "frame" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e2", Kind = "control",
                    From = new PipelinePortReference { NodeId = "gate1", Port = "productPresent" },
                    To = new PipelinePortReference { NodeId = "branch1", Port = "productPresent" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e3", Kind = "data",
                    From = new PipelinePortReference { NodeId = "branch1", Port = "acceptedFrame" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
                }
            ]
        };

    // ---- Fake implementations ----

    private sealed class FakeNodeActivator(params (string NodeId, INodeRunner Runner)[] runners) : IPipelineNodeActivator
    {
        private readonly Dictionary<string, INodeRunner> _runners =
            runners.ToDictionary(r => r.NodeId, r => r.Runner, StringComparer.OrdinalIgnoreCase);

        public async Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node,
            PipelineExecutionOptions options,
            CancellationToken cancellationToken)
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

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_index >= frames.Count)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            return Task.FromResult(NodeExecutionResult.Single("frame", PortValue.FromFrame(frames[_index++])));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class FakeGateRunner(string nodeId, bool gateOpen) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.Single("productPresent", PortValue.FromControl(new ControlSignal
            {
                SignalType = "boolean-gate",
                Value = gateOpen
            })));

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Thin wrapper to make IfPrimitiveNodeRunner accessible in tests (it's internal).</summary>
    private sealed class IfPrimitiveNodeRunnerWrapper(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            var frameInput = inputs.Get("frame");
            var controlInput = inputs.Get("productPresent");

            if (frameInput?.Frame is null || controlInput?.Control is null || !controlInput.Control.Value)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            return Task.FromResult(NodeExecutionResult.Single("acceptedFrame", frameInput));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class FakeSinkRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public List<IFrameEnvelope> ReceivedFrames { get; } = [];

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            var frameInput = inputs.Get("frame");
            if (frameInput?.Frame is not null)
            {
                ReceivedFrames.Add(frameInput.Frame);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class ContextCapturingSinkRunner(string nodeId, List<NodeExecutionContext?> captured) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is not null)
            {
                captured.Add(inputs.Context);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
