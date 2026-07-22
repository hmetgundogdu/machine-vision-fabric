using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;

namespace Mvf.Engine.Tests;

/// <summary>
/// Engine-crash resume (M2.5 C.2): captured node states are persisted to a checkpoint directory and,
/// on the next start, restored before the first cycle — so a fresh executor (a restarted process)
/// continues where the last left off. Because a checkpointable source's position is just its state, the
/// source resumes too, so the run is coherent (no re-processing). Verified with a real file store.
/// </summary>
public sealed class CheckpointResumeTests
{
    [Fact]
    public async Task Resume_FromPersistedCheckpoint_ContinuesSourceAndModuleState()
    {
        var checkpointDir = Path.Combine(Path.GetTempPath(), "mvf-ckpt-" + Guid.NewGuid().ToString("N"));
        var frames = Enumerable.Range(1, 6)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        try
        {
            // Run 1: a fresh source + counter process two frames, then the run stops (as if interrupted).
            var counter1 = new CountingRunner("counter1");
            var report1 = await RunAsync(new FakeCheckpointableSource("source1", frames), counter1, checkpointDir, maxCycles: 2);
            Assert.True(report1.Succeeded);
            Assert.Equal([1, 2], counter1.SeenSequences);
            Assert.Equal(2, counter1.Count);

            // Run 2: a brand-new executor with brand-new runners (a restarted process). On start it
            // restores from disk, so the source resumes at frame 3 and the counter continues from 2.
            var counter2 = new CountingRunner("counter1");
            var report2 = await RunAsync(new FakeCheckpointableSource("source1", frames), counter2, checkpointDir, maxCycles: 2);
            Assert.True(report2.Succeeded);
            Assert.Equal([3, 4], counter2.SeenSequences); // resumed, not restarted
            Assert.Equal(4, counter2.Count);               // continued from 2
        }
        finally
        {
            if (Directory.Exists(checkpointDir))
            {
                Directory.Delete(checkpointDir, recursive: true);
            }
        }
    }

    [Fact]
    public async Task CleanCompletion_ClearsTheCheckpoint_SoTheNextRunStartsFresh()
    {
        var checkpointDir = Path.Combine(Path.GetTempPath(), "mvf-ckpt-" + Guid.NewGuid().ToString("N"));
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        try
        {
            // Run to natural exhaustion (all 3 frames): the checkpoint is cleared on clean completion.
            var counter1 = new CountingRunner("counter1");
            await RunAsync(new FakeCheckpointableSource("source1", frames), counter1, checkpointDir, maxCycles: 0);
            Assert.Equal([1, 2, 3], counter1.SeenSequences);
            Assert.False(Directory.Exists(checkpointDir)); // cleared

            // The next run therefore starts fresh at frame 1.
            var counter2 = new CountingRunner("counter1");
            await RunAsync(new FakeCheckpointableSource("source1", frames), counter2, checkpointDir, maxCycles: 0);
            Assert.Equal([1, 2, 3], counter2.SeenSequences);
        }
        finally
        {
            if (Directory.Exists(checkpointDir))
            {
                Directory.Delete(checkpointDir, recursive: true);
            }
        }
    }

    private static Task<PipelineExecutionReport> RunAsync(
        FakeCheckpointableSource source, CountingRunner counter, string checkpointDir, int maxCycles)
    {
        var activator = new FakeActivator(("source1", source), ("counter1", counter));
        var executor = new PipelineGraphExecutor(activator);
        return executor.ExecuteAsync(BuildGraph(), new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = ".",
            MaxCycles = maxCycles,
            CheckpointIntervalCycles = 1,
            CheckpointDirectory = checkpointDir
        }, CancellationToken.None);
    }

    private static PipelineDefinition BuildGraph() => new()
    {
        Name = "resume-graph",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "integration-module", Category = "source",
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            },
            new PipelineNodeDefinition
            {
                Id = "counter1", Kind = "integration-module", Category = "classify",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition { Id = "e1", Kind = "data", From = new() { NodeId = "source1", Port = "frame" }, To = new() { NodeId = "counter1", Port = "frame" } }
        ]
    };

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

    // A source whose position IS its checkpointable state, so it resumes at the next unread frame.
    private sealed class FakeCheckpointableSource(string nodeId, IReadOnlyList<IFrameEnvelope> frames)
        : INodeRunner, ICheckpointable
    {
        private int _index;
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(_index >= frames.Count
                ? NodeExecutionResult.NoOutput
                : NodeExecutionResult.Single("frame", PortValue.FromFrame(frames[_index++])));

        public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
            Task.FromResult<byte[]?>(BitConverter.GetBytes(_index));

        public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken)
        {
            _index = BitConverter.ToInt32(state.Span);
            return Task.CompletedTask;
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class CountingRunner(string nodeId) : INodeRunner, ICheckpointable
    {
        private int _count;
        public int Count => _count;
        public List<int> SeenSequences { get; } = [];
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("frame")?.Frame is { } frame)
            {
                _count++;
                SeenSequences.Add(frame.SequenceNumber);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
            Task.FromResult<byte[]?>(BitConverter.GetBytes(_count));

        public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken)
        {
            _count = BitConverter.ToInt32(state.Span);
            return Task.CompletedTask;
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
