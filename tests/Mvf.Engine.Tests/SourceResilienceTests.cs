using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// A source that throws mid-stream should not always kill the run. These cover the failure-policy decorator
/// (<see cref="ResilientSourceRunner"/>): retry reconnects and continues, reconnect never gives up, and an
/// exhausted retry still rethrows so the run fails honestly. The engine default stays <c>fail</c>, so nothing
/// changes for a deployment that does not opt in.
/// </summary>
public sealed class SourceResilienceTests
{
    private static readonly SourceFailurePolicy FastRetry3 =
        new() { Mode = SourceFailureMode.Retry, MaxRetries = 3, BaseBackoffMs = 0, MaxBackoffMs = 0 };

    private static readonly SourceFailurePolicy FastReconnect =
        new() { Mode = SourceFailureMode.Reconnect, BaseBackoffMs = 0, MaxBackoffMs = 0 };

    private static NodeExecutionInputs EmptyInputs() =>
        new(new Dictionary<string, PortValue>(), new NodeExecutionContext { RunId = "r", CycleIndex = 0, CycleStartedAt = DateTime.UtcNow });

    [Fact]
    public async Task Retry_ReconnectsAndReturnsAFrame_WhenReadsRecover()
    {
        // Fails the first two reads, then yields a frame — like a camera that drops and comes back.
        var inner = new ScriptedSource("cam", throwsBeforeSuccess: 2);
        var runner = ResilientSourceRunnerFactory.Wrap(inner, FastRetry3, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(3, inner.ExecuteCalls);                 // 2 failures + 1 success
        Assert.Equal(1 + 2, inner.ActivateCalls);            // initial activation + 2 reconnects
    }

    [Fact]
    public async Task Retry_RethrowsOriginalError_WhenAttemptsAreExhausted()
    {
        var inner = new ScriptedSource("cam", throwsBeforeSuccess: int.MaxValue, message: "camera offline");
        var runner = ResilientSourceRunnerFactory.Wrap(inner, FastRetry3, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var ex = await Assert.ThrowsAsync<InvalidOperationException>(
            () => runner.ExecuteAsync(EmptyInputs(), CancellationToken.None));

        Assert.Contains("camera offline", ex.Message);
        Assert.Equal(3, inner.ReconnectCalls);               // exactly MaxRetries reconnects, then it gives up
    }

    [Fact]
    public async Task Reconnect_KeepsRetryingUntilItSucceeds()
    {
        // Ten failures then a frame: bounded retry(3) would give up, reconnect rides it out.
        var inner = new ScriptedSource("cam", throwsBeforeSuccess: 10);
        var runner = ResilientSourceRunnerFactory.Wrap(inner, FastReconnect, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(11, inner.ExecuteCalls);
    }

    [Fact]
    public async Task Exhaustion_IsNotAFailure_NoOutputPassesThroughWithoutRetry()
    {
        var inner = new ScriptedSource("cam", throwsBeforeSuccess: 0, exhaustAfter: 0); // returns NoOutput at once
        var runner = ResilientSourceRunnerFactory.Wrap(inner, FastReconnect, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.False(result.HasOutput);
        Assert.Equal(1, inner.ActivateCalls);                // initial only — a clean end of stream is never retried
    }

    [Fact]
    public async Task Cancellation_DuringBackoff_Propagates()
    {
        var inner = new ScriptedSource("cam", throwsBeforeSuccess: int.MaxValue);
        var policy = new SourceFailurePolicy { Mode = SourceFailureMode.Reconnect, BaseBackoffMs = 60_000, MaxBackoffMs = 60_000 };
        var runner = ResilientSourceRunnerFactory.Wrap(inner, policy, log: null);
        using var cts = new CancellationTokenSource();

        await runner.ActivateAsync(CancellationToken.None);
        var pending = runner.ExecuteAsync(EmptyInputs(), cts.Token);
        cts.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending);
    }

    [Fact]
    public void Wrap_KeepsRewindableCapability_OnlyWhenInnerHasIt()
    {
        var rewindable = ResilientSourceRunnerFactory.Wrap(new RewindableSource("a"), FastReconnect, null);
        var plain      = ResilientSourceRunnerFactory.Wrap(new ScriptedSource("b", 0), FastReconnect, null);

        Assert.IsAssignableFrom<IRewindableSource>(rewindable);
        Assert.IsNotAssignableFrom<IRewindableSource>(plain);
    }

    [Fact]
    public async Task EndToEnd_ExhaustedRetry_StillFailsTheRunHonestly()
    {
        // The whole point: retry buys reconnections, but a source that never recovers must still fail the run
        // (and not report a clean, empty success). Wire a wrapped always-throwing source through the executor.
        var inner = new ThrowingEveryTimeSource("source1", "unreachable");
        var wrapped = ResilientSourceRunnerFactory.Wrap(
            inner, new SourceFailurePolicy { Mode = SourceFailureMode.Retry, MaxRetries = 2, BaseBackoffMs = 0, MaxBackoffMs = 0 }, null);

        var report = await new PipelineGraphExecutor(new PresetActivator(("source1", wrapped), ("sink1", new SinkSource("sink1"))))
            .ExecuteAsync(BuildSourceToSink(), new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." }, CancellationToken.None);

        Assert.False(report.Succeeded);
        Assert.Equal(0, report.TotalCycles);
        Assert.Contains("unreachable", report.ErrorMessage);
        Assert.Equal(3, inner.ExecuteCalls);                 // 1 read + 2 retried reads, then the run fails
    }

    [Fact]
    public async Task HardRestart_RebuildsFromScratchAndDisposesTheBrokenRunner()
    {
        var broken  = new TrackingSource("cam", () => throw new InvalidOperationException("dead session"));
        var healthy = new TrackingSource("cam", () => NodeExecutionResult.Single("frame", PortValue.FromFrame(Frame(1))));
        var builds  = 0;
        Func<CancellationToken, Task<INodeRunner>> rebuild = _ => { builds++; return Task.FromResult<INodeRunner>(healthy); };

        var runner = ResilientSourceRunnerFactory.Wrap(broken, FastReconnect, log: null, rebuild);
        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(1, builds);                 // rebuilt once, from scratch (a fresh session)
        Assert.True(broken.Disposed);            // the dead runner was torn down
        Assert.False(healthy.Disposed);
        Assert.True(healthy.ActivateCalls >= 1); // the fresh runner was activated

        await runner.DisposeAsync();
        Assert.True(healthy.Disposed);           // the wrapper disposes its current (healthy) runner
    }

    [Fact]
    public async Task HardRestart_FailedRebuildsCountAsAttempts_ThenRecovers()
    {
        var healthy = new TrackingSource("cam", () => NodeExecutionResult.Single("frame", PortValue.FromFrame(Frame(1))));
        var builds  = 0;
        Func<CancellationToken, Task<INodeRunner>> rebuild = _ =>
        {
            builds++;
            if (builds <= 2) throw new InvalidOperationException("OpenSession failed"); // session not up yet
            return Task.FromResult<INodeRunner>(healthy);
        };
        var broken = new TrackingSource("cam", () => throw new InvalidOperationException("dead"));

        var runner = ResilientSourceRunnerFactory.Wrap(
            broken,
            new SourceFailurePolicy { Mode = SourceFailureMode.Retry, MaxRetries = 5, BaseBackoffMs = 0, MaxBackoffMs = 0 },
            log: null,
            rebuild);
        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(3, builds);                 // two failed OpenSessions counted, the third came up
    }

    // ---- policy parsing ----

    [Theory]
    [InlineData("fail", SourceFailureMode.Fail)]
    [InlineData("retry", SourceFailureMode.Retry)]
    [InlineData("reconnect", SourceFailureMode.Reconnect)]
    [InlineData("forever", SourceFailureMode.Reconnect)]
    public void FromConfig_StringShorthand_SetsMode(string token, SourceFailureMode expected)
    {
        var config = new JsonObject { ["onError"] = token };
        Assert.Equal(expected, SourceFailurePolicy.FromConfig(config, SourceFailurePolicy.Fail).Mode);
    }

    [Fact]
    public void FromConfig_Object_ReadsModeAndKnobs()
    {
        var config = new JsonObject
        {
            ["onError"] = new JsonObject { ["mode"] = "retry", ["maxRetries"] = 9, ["backoffMs"] = 250 }
        };

        var policy = SourceFailurePolicy.FromConfig(config, SourceFailurePolicy.Fail);

        Assert.Equal(SourceFailureMode.Retry, policy.Mode);
        Assert.Equal(9, policy.MaxRetries);
        Assert.Equal(250, policy.BaseBackoffMs);
    }

    [Fact]
    public void FromConfig_MissingOrUnknown_KeepsFallback()
    {
        var fallback = new SourceFailurePolicy { Mode = SourceFailureMode.Reconnect };
        Assert.Equal(fallback, SourceFailurePolicy.FromConfig(new JsonObject(), fallback));
        Assert.Equal(fallback.Mode, SourceFailurePolicy.FromConfig(new JsonObject { ["onError"] = "nonsense" }, fallback).Mode);
    }

    [Fact]
    public void BackoffFor_IsExponential_AndCapped()
    {
        var p = new SourceFailurePolicy { BaseBackoffMs = 100, MaxBackoffMs = 500 };
        Assert.Equal(100, p.BackoffFor(1).TotalMilliseconds);
        Assert.Equal(200, p.BackoffFor(2).TotalMilliseconds);
        Assert.Equal(400, p.BackoffFor(3).TotalMilliseconds);
        Assert.Equal(500, p.BackoffFor(4).TotalMilliseconds);   // capped
        Assert.Equal(500, p.BackoffFor(9).TotalMilliseconds);
    }

    // ---- fakes ----

    private static IFrameEnvelope Frame(int i) => new BinaryFrameEnvelope("cam", i, $"f{i}.bmp", [(byte)i], "image/bmp");

    /// <summary>Throws on the first N reads, then yields one frame (or exhausts). Plain INodeRunner (not rewindable).</summary>
    private sealed class ScriptedSource(string nodeId, int throwsBeforeSuccess, string message = "read failed", int exhaustAfter = 1)
        : INodeRunner
    {
        private int _thrown;
        public int ExecuteCalls { get; private set; }
        public int ActivateCalls { get; private set; }
        public int ReconnectCalls => ActivateCalls - 1;
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken)
        {
            ActivateCalls++;
            return Task.CompletedTask;
        }

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            ExecuteCalls++;
            if (_thrown < throwsBeforeSuccess)
            {
                _thrown++;
                throw new InvalidOperationException(message);
            }
            return Task.FromResult(exhaustAfter <= 0
                ? NodeExecutionResult.NoOutput
                : NodeExecutionResult.Single("frame", PortValue.FromFrame(Frame(ExecuteCalls))));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class RewindableSource(string nodeId) : INodeRunner, IRewindableSource
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);
        public Task RewindAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Tracks activation/disposal and reads via a supplied delegate (which may throw). Used for hard-restart tests.</summary>
    private sealed class TrackingSource(string nodeId, Func<NodeExecutionResult> read) : INodeRunner
    {
        public int ActivateCalls { get; private set; }
        public bool Disposed { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) { ActivateCalls++; return Task.CompletedTask; }
        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(read());
        public ValueTask DisposeAsync() { Disposed = true; return ValueTask.CompletedTask; }
    }

    private sealed class ThrowingEveryTimeSource(string nodeId, string message) : INodeRunner
    {
        public int ExecuteCalls { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            ExecuteCalls++;
            throw new InvalidOperationException(message);
        }
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class SinkSource(string nodeId) : INodeRunner
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

    private sealed class PresetActivator(params (string NodeId, INodeRunner Runner)[] runners) : IPipelineNodeActivator
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
}
