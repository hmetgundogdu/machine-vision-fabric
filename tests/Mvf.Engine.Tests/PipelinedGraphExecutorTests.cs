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

        // The two modes must agree on what the run produced, not merely both succeed.
        var serial = await RunAsync(PipelineExecutionMode.Serial, frameCount: 5);
        Assert.Equal(serial.TotalCycles, report.TotalCycles);
        Assert.Equal(serial.AcceptedCycles, report.AcceptedCycles);
    }

    [Fact]
    public async Task Pipelined_CountsAcceptedCyclesNotSinkExecutions()
    {
        // Two sinks fed by a fork: four frames must read as 4 accepted cycles, not 8 sink runs. The field
        // means "cycles where at least one sink received output", and it has to mean that in both modes.
        var frames = Enumerable.Range(1, 4)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var definition = BuildForkJoin();
        var twoSinks = new PipelineDefinition
        {
            Name = definition.Name,
            Nodes =
            [
                definition.Nodes[0],
                definition.Nodes[1],
                new PipelineNodeDefinition
                {
                    Id = "sinkA", Kind = "integration-module", Category = "output", ModuleId = "mvf.file-sink",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "sinkB", Kind = "integration-module", Category = "output", ModuleId = "mvf.file-sink",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
                }
            ],
            Edges =
            [
                definition.Edges[0],
                new PipelineEdgeDefinition
                {
                    Id = "e2", Kind = "data",
                    From = new PipelinePortReference { NodeId = "fork1", Port = "a" },
                    To = new PipelinePortReference { NodeId = "sinkA", Port = "frame" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e3", Kind = "data",
                    From = new PipelinePortReference { NodeId = "fork1", Port = "b" },
                    To = new PipelinePortReference { NodeId = "sinkB", Port = "frame" }
                }
            ]
        };

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("fork1", new ForkRunner("fork1")),
            ("sinkA", new RecordingSinkRunner("sinkA")),
            ("sinkB", new RecordingSinkRunner("sinkB")));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            twoSinks,
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".", ExecutionMode = PipelineExecutionMode.Pipelined
            },
            CancellationToken.None);

        Assert.True(report.Succeeded, report.ErrorMessage);
        Assert.Equal(4, report.TotalCycles);
        Assert.Equal(4, report.AcceptedCycles);
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
    public async Task Pipelined_JoinsTwoInputsFromTheSameCycle()
    {
        var join = new PairRecordingJoinRunner("join1");
        var frames = Enumerable.Range(1, 25)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("fork1", new ForkRunner("fork1")),
            ("join1", join));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            BuildForkJoin(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".",
                ExecutionMode = PipelineExecutionMode.Pipelined,
                EdgeQueueCapacity = 2   // deliberately shallow: the branches must not drift apart
            },
            CancellationToken.None);

        Assert.True(report.Succeeded, report.ErrorMessage);
        Assert.Equal(25, join.Pairs.Count);
        // Every join must see the two halves of one frame, in source order — never frame N's left with
        // frame N-1's right, which is precisely what an uncorrelated queue pairing would produce.
        Assert.All(join.Pairs, pair => Assert.Equal(pair.Left, pair.Right));
        Assert.Equal(Enumerable.Range(1, 25), join.Pairs.Select(p => p.Left!.Value));
    }

    [Fact]
    public async Task Pipelined_UntakenBranchDoesNotStallTheJoin()
    {
        var join = new PairRecordingJoinRunner("join1");
        var frames = Enumerable.Range(1, 20)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        // Emits on exactly one of its two output ports per frame — the shape of a switch.
        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("fork1", new AlternatingBranchRunner("fork1")),
            ("join1", join));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            BuildForkJoin(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".",
                ExecutionMode = PipelineExecutionMode.Pipelined,
                EdgeQueueCapacity = 2
            },
            CancellationToken.None);

        // Without a void marker on the untaken branch this deadlocks: the join waits forever for a value
        // that was never produced. It must instead run once per frame with a single side present.
        Assert.True(report.Succeeded, report.ErrorMessage);
        Assert.Equal(20, join.Pairs.Count);
        Assert.All(join.Pairs, pair => Assert.True(
            (pair.Left is null) ^ (pair.Right is null), "exactly one branch should carry each frame"));
        Assert.Equal(
            Enumerable.Range(1, 20),
            join.Pairs.Select(p => (p.Left ?? p.Right)!.Value));
    }

    [Fact]
    public async Task Pipelined_RejectsTwoEdgesIntoOnePort()
    {
        var forkJoin = BuildForkJoin();
        // Point both fork outputs at the same input port — an ambiguous producer race.
        var definition = new PipelineDefinition
        {
            Name = forkJoin.Name,
            Nodes = forkJoin.Nodes,
            Edges =
            [
                .. forkJoin.Edges.Take(2),
                new PipelineEdgeDefinition
                {
                    Id = "e3", Kind = "data",
                    From = new PipelinePortReference { NodeId = "fork1", Port = "b" },
                    To = new PipelinePortReference { NodeId = "join1", Port = "left" }
                }
            ]
        };

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", [])),
            ("fork1", new ForkRunner("fork1")),
            ("join1", new PairRecordingJoinRunner("join1")));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            definition,
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".", ExecutionMode = PipelineExecutionMode.Pipelined
            },
            CancellationToken.None);

        Assert.False(report.Succeeded);
        Assert.Contains("input port", report.ErrorMessage);
        Assert.Contains("serial mode", report.ErrorMessage);
    }

    [Fact]
    public async Task Pipelined_EpochBarrier_CapturesWithNothingInFlight()
    {
        var sink = new StatefulCountingRunner("sink1");
        var frames = Enumerable.Range(1, 12)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("sink1", sink));

        var report = await new PipelinedGraphExecutor(activator).ExecuteAsync(
            BuildSourceToSink(),
            new PipelineExecutionOptions
            {
                PackageRoot = ".", IntegrationsRoot = ".",
                ExecutionMode = PipelineExecutionMode.Pipelined,
                CheckpointIntervalCycles = 4,
                EdgeQueueCapacity = 4   // deep enough for frames to still be queued if the drain is missing
            },
            CancellationToken.None);

        Assert.True(report.Succeeded, report.ErrorMessage);

        // The point of the barrier: at each capture the sink has consumed *everything* the source emitted,
        // so the snapshot is torn-free. Without the drain the sink would lag by whatever sits in the queue
        // and these counts would come out below 4, 8, 12.
        Assert.Equal([4, 8, 12], sink.CountsAtCheckpoint);
    }

    [Fact]
    public async Task Pipelined_PersistsCheckpointAndResumesFromIt()
    {
        var resumeDir = Path.Combine(Path.GetTempPath(), $"mvf-pipelined-resume-{Guid.NewGuid():N}");
        Directory.CreateDirectory(resumeDir);

        try
        {
            var frames = Enumerable.Range(1, 20)
                .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
                .ToArray();

            PipelineExecutionOptions Options(int maxCycles) => new()
            {
                PackageRoot = ".", IntegrationsRoot = ".",
                ExecutionMode = PipelineExecutionMode.Pipelined,
                CheckpointIntervalCycles = 2,
                CheckpointDirectory = resumeDir,
                MaxCycles = maxCycles
            };

            // Interrupted by MaxCycles, so the source is not exhausted and the checkpoint must survive.
            var first = new StatefulCountingRunner("sink1");
            var firstReport = await new PipelinedGraphExecutor(new FakeActivator(
                    ("source1", new ListSourceRunner("source1", frames)),
                    ("sink1", first)))
                .ExecuteAsync(BuildSourceToSink(), Options(maxCycles: 4), CancellationToken.None);

            Assert.True(firstReport.Succeeded, firstReport.ErrorMessage);
            Assert.Equal(4, first.Count);
            Assert.True(File.Exists(Path.Combine(resumeDir, "sink1.state")), "an interrupted run must stay resumable");

            // A fresh executor and a fresh runner: the count can only be right if the state was restored.
            var second = new StatefulCountingRunner("sink1");
            var secondReport = await new PipelinedGraphExecutor(new FakeActivator(
                    ("source1", new ListSourceRunner("source1", frames)),
                    ("sink1", second)))
                .ExecuteAsync(BuildSourceToSink(), Options(maxCycles: 2), CancellationToken.None);

            Assert.True(secondReport.Succeeded, secondReport.ErrorMessage);
            Assert.Equal(6, second.Count);   // 4 restored + 2 more
        }
        finally
        {
            try { Directory.Delete(resumeDir, recursive: true); } catch { /* best effort */ }
        }
    }

    [Fact]
    public async Task Pipelined_ClearsCheckpointWhenTheSourceRunsOut()
    {
        var resumeDir = Path.Combine(Path.GetTempPath(), $"mvf-pipelined-clear-{Guid.NewGuid():N}");
        Directory.CreateDirectory(resumeDir);

        try
        {
            var frames = Enumerable.Range(1, 4)
                .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
                .ToArray();

            var report = await new PipelinedGraphExecutor(new FakeActivator(
                    ("source1", new ListSourceRunner("source1", frames)),
                    ("sink1", new StatefulCountingRunner("sink1"))))
                .ExecuteAsync(
                    BuildSourceToSink(),
                    new PipelineExecutionOptions
                    {
                        PackageRoot = ".", IntegrationsRoot = ".",
                        ExecutionMode = PipelineExecutionMode.Pipelined,
                        CheckpointIntervalCycles = 2,
                        CheckpointDirectory = resumeDir
                    },
                    CancellationToken.None);

            Assert.True(report.Succeeded, report.ErrorMessage);
            Assert.False(File.Exists(Path.Combine(resumeDir, "sink1.state")),
                "a fully-consumed run has nothing to resume");
        }
        finally
        {
            try { Directory.Delete(resumeDir, recursive: true); } catch { /* best effort */ }
        }
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

    /// <summary>source → fork → (left, right) → join: the smallest graph with a real multi-input join.</summary>
    private static PipelineDefinition BuildForkJoin() => new()
    {
        Name = "fork-join",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "runtime-builtin", Category = "source",
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            },
            new PipelineNodeDefinition
            {
                Id = "fork1", Kind = "embedded-primitive", Category = "flow-control", PrimitiveType = "fork",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }],
                Outputs =
                [
                    new PipelinePortDefinition { Name = "a", Channel = "data", DataType = "data/frame" },
                    new PipelinePortDefinition { Name = "b", Channel = "data", DataType = "data/frame" }
                ]
            },
            new PipelineNodeDefinition
            {
                Id = "join1", Kind = "integration-module", Category = "output", ModuleId = "mvf.file-sink",
                Inputs =
                [
                    new PipelinePortDefinition { Name = "left", Channel = "data", DataType = "data/frame" },
                    new PipelinePortDefinition { Name = "right", Channel = "data", DataType = "data/frame" }
                ]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition
            {
                Id = "e1", Kind = "data",
                From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                To = new PipelinePortReference { NodeId = "fork1", Port = "frame" }
            },
            new PipelineEdgeDefinition
            {
                Id = "e2", Kind = "data",
                From = new PipelinePortReference { NodeId = "fork1", Port = "a" },
                To = new PipelinePortReference { NodeId = "join1", Port = "left" }
            },
            new PipelineEdgeDefinition
            {
                Id = "e3", Kind = "data",
                From = new PipelinePortReference { NodeId = "fork1", Port = "b" },
                To = new PipelinePortReference { NodeId = "join1", Port = "right" }
            }
        ]
    };

    // ---- fakes ----

    /// <summary>Re-emits its input frame on both output ports.</summary>
    private sealed class ForkRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            var frame = inputs.Get("frame");
            return Task.FromResult(frame?.Frame is null
                ? NodeExecutionResult.NoOutput
                : NodeExecutionResult.FromPairs(("a", frame), ("b", frame)));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Emits on one output port or the other, alternating — the shape of a switch.</summary>
    private sealed class AlternatingBranchRunner(string nodeId) : INodeRunner
    {
        private int _seen;
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            var frame = inputs.Get("frame");
            if (frame?.Frame is null)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            var port = _seen++ % 2 == 0 ? "a" : "b";
            return Task.FromResult(NodeExecutionResult.Single(port, frame));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>
    /// A stateful leaf: counts the frames it consumed, and its checkpoint is that count. It also records
    /// the count at each capture, which is how "nothing was in flight" becomes observable from a test.
    /// </summary>
    private sealed class StatefulCountingRunner(string nodeId) : INodeRunner, ICheckpointable
    {
        private readonly List<int> _countsAtCheckpoint = [];
        public string NodeId { get; } = nodeId;
        public int Count { get; private set; }
        public IReadOnlyList<int> CountsAtCheckpoint => _countsAtCheckpoint;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is not null)
            {
                Count++;
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken)
        {
            _countsAtCheckpoint.Add(Count);
            return Task.FromResult<byte[]?>(BitConverter.GetBytes(Count));
        }

        public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken)
        {
            Count = BitConverter.ToInt32(state.Span);
            return Task.CompletedTask;
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Records what arrived on each side of the join, so pairing can be asserted directly.</summary>
    private sealed class PairRecordingJoinRunner(string nodeId) : INodeRunner
    {
        private readonly List<(int? Left, int? Right)> _pairs = [];
        public string NodeId { get; } = nodeId;
        public IReadOnlyList<(int? Left, int? Right)> Pairs => _pairs;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            _pairs.Add((inputs.Get("left")?.Frame?.SequenceNumber, inputs.Get("right")?.Frame?.SequenceNumber));
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

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
