using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// The opt-in pipelined executor (phase 1, step 1): stage tasks over bounded per-edge queues. These pin
/// that it agrees with the serial executor on what a run produced, and that it <b>refuses</b> the graph
/// shapes whose pipelined semantics are not built yet rather than running them on a guess.
/// </summary>
public sealed class PipelinedGraphExecutorTests
{
    [Fact]
    public async Task Pipelined_LinearGraph_MatchesSerialResult()
    {
        var report = await RunAsync(PipelineExecutionMode.Pipelined, frameCount: 5);

        Assert.True(report.Succeeded, report.ErrorMessage);
        Assert.Equal(5, report.TotalCycles);
        Assert.Equal(5, report.AcceptedCycles);          // the sink saw every frame
        Assert.Equal(5, report.NodeStats["sink1"].TotalCycles);

        var serial = await RunAsync(PipelineExecutionMode.Serial, frameCount: 5);
        Assert.Equal(serial.TotalCycles, report.TotalCycles);
        Assert.Equal(serial.AcceptedCycles, report.AcceptedCycles);
    }

    [Fact]
    public async Task Pipelined_PreservesSourceOrderAtTheSink()
    {
        var sink = new RecordingSinkRunner("sink1");
        var frames = Enumerable.Range(1, 20)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("sink1", sink));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            BuildSourceToSink(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".", ExecutionMode = PipelineExecutionMode.Pipelined
            },
            CancellationToken.None);

        Assert.True(report.Succeeded, report.ErrorMessage);
        // One instance per node plus FIFO queues: the sink must see 1..20 in order, no reorder buffer needed.
        Assert.Equal(Enumerable.Range(1, 20), sink.SeenSequences);
    }

    [Fact]
    public async Task Pipelined_RejectsMultiInputJoin()
    {
        var definition = BuildSourceToSink();
        definition.Nodes[1].Inputs =
        [
            new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" },
            new PipelinePortDefinition { Name = "class", Channel = "control", DataType = "control/classification" }
        ];
        definition.Edges =
        [
            .. definition.Edges,
            new PipelineEdgeDefinition
            {
                Id = "e2", Kind = "control",
                From = new PipelinePortReference { NodeId = "source1", Port = "class" },
                To = new PipelinePortReference { NodeId = "sink1", Port = "class" }
            }
        ];

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", [])),
            ("sink1", new RecordingSinkRunner("sink1")));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            definition,
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".", ExecutionMode = PipelineExecutionMode.Pipelined
            },
            CancellationToken.None);

        Assert.False(report.Succeeded);
        Assert.Contains("incoming edges", report.ErrorMessage);
        Assert.Contains("serial mode", report.ErrorMessage);
    }

    [Fact]
    public async Task Pipelined_RejectsCheckpointing()
    {
        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", [])),
            ("sink1", new RecordingSinkRunner("sink1")));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            BuildSourceToSink(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".",
                ExecutionMode = PipelineExecutionMode.Pipelined,
                CheckpointIntervalCycles = 2
            },
            CancellationToken.None);

        // Silently ignoring the option is exactly the failure mode this refuses to repeat.
        Assert.False(report.Succeeded);
        Assert.Contains("checkpointing", report.ErrorMessage);
    }

    // ---- helpers ----

    private static async Task<PipelineExecutionReport> RunAsync(PipelineExecutionMode mode, int frameCount)
    {
        var frames = Enumerable.Range(1, frameCount)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("sink1", new RecordingSinkRunner("sink1")));

        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".", IntegrationsRoot = ".", ExecutionMode = mode
        };

        return mode == PipelineExecutionMode.Pipelined
            ? await new PipelinedGraphExecutor(activator).ExecuteAsync(BuildSourceToSink(), options, CancellationToken.None)
            : await new PipelineGraphExecutor(activator).ExecuteAsync(BuildSourceToSink(), options, CancellationToken.None);
    }

    private static PipelineDefinition BuildSourceToSink() => new()
    {
        Name = "source-to-sink",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "runtime-builtin", Category = "source",
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            },
            new PipelineNodeDefinition
            {
                Id = "sink1", Kind = "integration-module", Category = "output", ModuleId = "mvf.file-sink",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition
            {
                Id = "e1", Kind = "data",
                From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
            }
        ]
    };

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

    private sealed class ListSourceRunner(string nodeId, IReadOnlyList<IFrameEnvelope> frames) : INodeRunner
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

    private sealed class RecordingSinkRunner(string nodeId) : INodeRunner
    {
        private readonly List<int> _seen = [];
        public string NodeId { get; } = nodeId;
        public IReadOnlyList<int> SeenSequences => _seen;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is { } frame)
            {
                _seen.Add(frame.SequenceNumber);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
