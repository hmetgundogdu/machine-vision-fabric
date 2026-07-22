using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;

namespace Mvf.Engine.Tests;

/// <summary>
/// The executor snapshots stateful workers at cycle boundaries (every N cycles) so a supervised worker
/// always has a recent state to recover with. A runner that reports no state is checkpointed once and
/// then skipped. Verified with fake runners (no Python needed).
/// </summary>
public sealed class PipelineGraphExecutorCheckpointTests
{
    [Fact]
    public async Task Execute_WithCheckpointInterval_SnapshotsStatefulEveryNCyclesAndSkipsStateless()
    {
        var frames = Enumerable.Range(1, 6)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var stateful = new CheckpointCountingRunner("stateful", state: [1]);
        var stateless = new CheckpointCountingRunner("stateless", state: null);

        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("stateful", stateful),
            ("stateless", stateless));

        var executor = new PipelineGraphExecutor(activator);
        var options = new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = ".",
            CheckpointIntervalCycles = 2
        };

        var report = await executor.ExecuteAsync(BuildGraph(), options, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(6, report.TotalCycles);
        Assert.Equal(3, stateful.CheckpointCount);   // cycles 2, 4, 6
        Assert.Equal(1, stateless.CheckpointCount);   // captured once, then skipped forever
    }

    [Fact]
    public async Task Execute_WithoutCheckpointInterval_NeverCheckpoints()
    {
        var frames = Enumerable.Range(1, 4)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var stateful = new CheckpointCountingRunner("stateful", state: [1]);
        var activator = new FakeActivator(
            ("source1", new FakeSourceRunner("source1", frames)),
            ("stateful", stateful),
            ("stateless", new CheckpointCountingRunner("stateless", state: null)));

        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(BuildGraph(), new PipelineExecutionOptions
        {
            PackageRoot = ".",
            IntegrationsRoot = "."
        }, CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(0, stateful.CheckpointCount); // interval 0 = disabled
    }

    private static PipelineDefinition BuildGraph() => new()
    {
        Name = "checkpoint-graph",
        Nodes =
        [
            new PipelineNodeDefinition
            {
                Id = "source1", Kind = "integration-module", Category = "source",
                Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", AllowMultipleEdges = true }]
            },
            new PipelineNodeDefinition
            {
                Id = "stateful", Kind = "integration-module", Category = "classify",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            },
            new PipelineNodeDefinition
            {
                Id = "stateless", Kind = "integration-module", Category = "classify",
                Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
            }
        ],
        Edges =
        [
            new PipelineEdgeDefinition { Id = "e1", Kind = "data", From = new() { NodeId = "source1", Port = "frame" }, To = new() { NodeId = "stateful", Port = "frame" } },
            new PipelineEdgeDefinition { Id = "e2", Kind = "data", From = new() { NodeId = "source1", Port = "frame" }, To = new() { NodeId = "stateless", Port = "frame" } }
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

    private sealed class CheckpointCountingRunner(string nodeId, byte[]? state) : INodeRunner, ICheckpointable
    {
        public int CheckpointCount { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            _ = inputs.Get("frame");
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken)
        {
            CheckpointCount++;
            return Task.FromResult(state);
        }

        public Task RestoreAsync(ReadOnlyMemory<byte> restored, CancellationToken cancellationToken) => Task.CompletedTask;

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
