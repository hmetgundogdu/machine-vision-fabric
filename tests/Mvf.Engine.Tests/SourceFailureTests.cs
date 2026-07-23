using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// A source that fails and a source that runs out both surface as "no frame this cycle". Conflating them
/// let a camera that never connected finish as a clean, successful run of zero cycles — and clear the
/// resume checkpoint on the way out. These tests hold the two apart.
/// </summary>
public sealed class SourceFailureTests
{
    [Fact]
    public async Task Execute_WhenSourceThrows_FailsTheRunWithTheReason()
    {
        var activator = new FakeActivator(
            ("source1", new ThrowingSourceRunner("source1", "Unable to connect to the remote server")),
            ("sink1", new FrameConsumingRunner("sink1")));

        var report = await new PipelineGraphExecutor(activator).ExecuteAsync(
            BuildSourceToSink(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
            CancellationToken.None);

        Assert.False(report.Succeeded);
        Assert.Equal(0, report.TotalCycles);
        Assert.NotNull(report.ErrorMessage);
        Assert.Contains("source1", report.ErrorMessage);
        Assert.Contains("Unable to connect to the remote server", report.ErrorMessage);
        Assert.Equal(1, report.NodeStats["source1"].FaultedCycles);
    }

    [Fact]
    public async Task Execute_WhenSourceIsExhausted_StillSucceeds()
    {
        var frames = Enumerable.Range(1, 2)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.bmp", [(byte)i], "image/bmp"))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new ListSourceRunner("source1", frames)),
            ("sink1", new FrameConsumingRunner("sink1")));

        var report = await new PipelineGraphExecutor(activator).ExecuteAsync(
            BuildSourceToSink(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
            CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(2, report.TotalCycles);
        Assert.Null(report.ErrorMessage);
    }

    [Fact]
    public async Task Execute_WhenSourceThrows_KeepsTheCheckpointButClearsItOnCleanExhaustion()
    {
        var resumeDir = Path.Combine(Path.GetTempPath(), $"mvf-source-failure-{Guid.NewGuid():N}");
        Directory.CreateDirectory(resumeDir);
        var stateFile = Path.Combine(resumeDir, "source1.state");

        try
        {
            await File.WriteAllBytesAsync(stateFile, [1, 2, 3]);

            // A failed source has not consumed its stream — the resume point must survive.
            var failing = new FakeActivator(
                ("source1", new ThrowingSourceRunner("source1", "camera offline")),
                ("sink1", new FrameConsumingRunner("sink1")));

            var failed = await new PipelineGraphExecutor(failing).ExecuteAsync(
                BuildSourceToSink(),
                new PipelineExecutionOptions
                {
                    PackageRoot = ".", IntegrationsRoot = ".", CheckpointDirectory = resumeDir
                },
                CancellationToken.None);

            Assert.False(failed.Succeeded);
            Assert.True(File.Exists(stateFile), "a failed source must stay resumable");

            // A stream that genuinely ran out has nothing left to resume — that one is cleared.
            var exhausting = new FakeActivator(
                ("source1", new ListSourceRunner("source1", [])),
                ("sink1", new FrameConsumingRunner("sink1")));

            var completed = await new PipelineGraphExecutor(exhausting).ExecuteAsync(
                BuildSourceToSink(),
                new PipelineExecutionOptions
                {
                    PackageRoot = ".", IntegrationsRoot = ".", CheckpointDirectory = resumeDir
                },
                CancellationToken.None);

            Assert.True(completed.Succeeded);
            Assert.False(File.Exists(stateFile), "a fully-consumed run has nothing to resume");
        }
        finally
        {
            try { Directory.Delete(resumeDir, recursive: true); } catch { /* best effort */ }
        }
    }

    // ---- helpers ----

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

    /// <summary>
    /// A source whose read fails, the way a camera session faults its frame channel on a failed connect.
    /// </summary>
    private sealed class ThrowingSourceRunner(string nodeId, string message) : INodeRunner
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            throw new InvalidOperationException(message);

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
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
