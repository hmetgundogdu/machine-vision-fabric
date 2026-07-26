using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// A node that throws should not always take its default path (a source ending the run, a mid-graph node
/// skipping). These cover the failure-policy decorator (<see cref="ResilientNodeRunner"/>): a bounded restart
/// recovers within its limit or rethrows once spent, an unbounded restart never gives up, and a hard restart
/// rebuilds the node from scratch. The wrapper is node-agnostic — the same decorator serves a source, a
/// classifier, or a sink. The engine default stays <c>fail</c>, so nothing changes without opting in.
/// </summary>
public sealed class NodeResilienceTests
{
    private static readonly NodeFailurePolicy FastRestart3 =
        new() { Mode = NodeFailureMode.Restart, Limit = 3, BaseBackoffMs = 0, MaxBackoffMs = 0 };

    private static readonly NodeFailurePolicy FastRestartForever =
        new() { Mode = NodeFailureMode.Restart, Limit = 0, BaseBackoffMs = 0, MaxBackoffMs = 0 };

    private static NodeExecutionInputs EmptyInputs() =>
        new(new Dictionary<string, PortValue>(), new NodeExecutionContext { RunId = "r", CycleIndex = 0, CycleStartedAt = DateTime.UtcNow });

    [Fact]
    public async Task Restart_Bounded_RecoversWithinItsLimit()
    {
        // Fails the first two runs, then yields a result — like a node that faults and comes back.
        var inner = new ScriptedNode("cam", throwsBeforeSuccess: 2);
        var runner = ResilientNodeRunnerFactory.Wrap(inner, FastRestart3, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(3, inner.ExecuteCalls);                 // 2 failures + 1 success
        Assert.Equal(1 + 2, inner.ActivateCalls);            // initial activation + 2 restarts
    }

    [Fact]
    public async Task Restart_Bounded_RethrowsOriginalError_WhenLimitIsExhausted()
    {
        var inner = new ScriptedNode("cam", throwsBeforeSuccess: int.MaxValue, message: "node offline");
        var runner = ResilientNodeRunnerFactory.Wrap(inner, FastRestart3, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var ex = await Assert.ThrowsAsync<InvalidOperationException>(
            () => runner.ExecuteAsync(EmptyInputs(), CancellationToken.None));

        Assert.Contains("node offline", ex.Message);
        Assert.Equal(3, inner.RestartCalls);                 // exactly Limit restarts, then it gives up
    }

    [Fact]
    public async Task Restart_Unbounded_KeepsGoingUntilItSucceeds()
    {
        // Ten failures then a result: a limit of 3 would give up, forever rides it out.
        var inner = new ScriptedNode("cam", throwsBeforeSuccess: 10);
        var runner = ResilientNodeRunnerFactory.Wrap(inner, FastRestartForever, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(11, inner.ExecuteCalls);
    }

    [Fact]
    public async Task EmptyResult_IsNotAFailure_NoOutputPassesThroughWithoutRestart()
    {
        var inner = new ScriptedNode("cam", throwsBeforeSuccess: 0, exhaustAfter: 0); // returns NoOutput at once
        var runner = ResilientNodeRunnerFactory.Wrap(inner, FastRestartForever, log: null);

        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.False(result.HasOutput);
        Assert.Equal(1, inner.ActivateCalls);                // initial only — an empty result is never restarted
    }

    [Fact]
    public async Task Cancellation_DuringBackoff_Propagates()
    {
        var inner = new ScriptedNode("cam", throwsBeforeSuccess: int.MaxValue);
        var policy = new NodeFailurePolicy { Mode = NodeFailureMode.Restart, BaseBackoffMs = 60_000, MaxBackoffMs = 60_000 };
        var runner = ResilientNodeRunnerFactory.Wrap(inner, policy, log: null);
        using var cts = new CancellationTokenSource();

        await runner.ActivateAsync(CancellationToken.None);
        var pending = runner.ExecuteAsync(EmptyInputs(), cts.Token);
        cts.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending);
    }

    [Fact]
    public void Wrap_KeepsRewindableCapability_OnlyWhenInnerHasIt()
    {
        var rewindable = ResilientNodeRunnerFactory.Wrap(new RewindableNode("a"), FastRestartForever, null);
        var plain      = ResilientNodeRunnerFactory.Wrap(new ScriptedNode("b", 0), FastRestartForever, null);

        Assert.IsAssignableFrom<IRewindableSource>(rewindable);
        Assert.IsNotAssignableFrom<IRewindableSource>(plain);
    }

    [Fact]
    public async Task HardRestart_RebuildsFromScratchAndDisposesTheBrokenRunner()
    {
        // The same hard restart serves any node — here a stand-in for a sink or classifier that faults.
        var broken  = new TrackingNode("sink", () => throw new InvalidOperationException("dead session"));
        var healthy = new TrackingNode("sink", () => NodeExecutionResult.Single("frame", PortValue.FromFrame(Frame(1))));
        var builds  = 0;
        Func<CancellationToken, Task<INodeRunner>> rebuild = _ => { builds++; return Task.FromResult<INodeRunner>(healthy); };

        var runner = ResilientNodeRunnerFactory.Wrap(broken, FastRestartForever, log: null, rebuild);
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
        var healthy = new TrackingNode("cam", () => NodeExecutionResult.Single("frame", PortValue.FromFrame(Frame(1))));
        var builds  = 0;
        Func<CancellationToken, Task<INodeRunner>> rebuild = _ =>
        {
            builds++;
            if (builds <= 2) throw new InvalidOperationException("OpenSession failed"); // session not up yet
            return Task.FromResult<INodeRunner>(healthy);
        };
        var broken = new TrackingNode("cam", () => throw new InvalidOperationException("dead"));

        var runner = ResilientNodeRunnerFactory.Wrap(
            broken,
            new NodeFailurePolicy { Mode = NodeFailureMode.Restart, Limit = 5, BaseBackoffMs = 0, MaxBackoffMs = 0 },
            log: null,
            rebuild);
        await runner.ActivateAsync(CancellationToken.None);
        var result = await runner.ExecuteAsync(EmptyInputs(), CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.Equal(3, builds);                 // two failed OpenSessions counted, the third came up
    }

    [Fact]
    public async Task EndToEnd_ExhaustedRestart_StillFailsTheRunHonestly()
    {
        // Restart buys recoveries, but a source that never recovers must still fail the run (and not report a
        // clean, empty success). Wire a wrapped always-throwing source through the executor.
        var inner = new ThrowingEveryTimeNode("source1", "unreachable");
        var wrapped = ResilientNodeRunnerFactory.Wrap(
            inner, new NodeFailurePolicy { Mode = NodeFailureMode.Restart, Limit = 2, BaseBackoffMs = 0, MaxBackoffMs = 0 }, null);

        var report = await new PipelineGraphExecutor(new PresetActivator(("source1", wrapped), ("sink1", new SinkNode("sink1"))))
            .ExecuteAsync(BuildSourceToSink(), new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." }, CancellationToken.None);

        Assert.False(report.Succeeded);
        Assert.Equal(0, report.TotalCycles);
        Assert.Contains("unreachable", report.ErrorMessage);
        Assert.Equal(3, inner.ExecuteCalls);                 // 1 run + 2 restarted runs, then the run fails
    }

    // ---- policy parsing ----

    [Theory]
    [InlineData("fail", NodeFailureMode.Fail)]
    [InlineData("restart", NodeFailureMode.Restart)]
    [InlineData("retry", NodeFailureMode.Restart)]       // legacy alias
    [InlineData("reconnect", NodeFailureMode.Restart)]   // legacy alias
    [InlineData("forever", NodeFailureMode.Restart)]     // legacy alias
    public void FromConfig_StringShorthand_SetsMode(string token, NodeFailureMode expected)
    {
        var config = new JsonObject { ["onError"] = token };
        Assert.Equal(expected, NodeFailurePolicy.FromConfig(config, NodeFailurePolicy.Fail).Mode);
    }

    [Fact]
    public void FromConfig_Object_ReadsModeAndKnobs()
    {
        var config = new JsonObject
        {
            ["onError"] = new JsonObject { ["mode"] = "restart", ["limit"] = 9, ["backoffMs"] = 250 }
        };

        var policy = NodeFailurePolicy.FromConfig(config, NodeFailurePolicy.Fail);

        Assert.Equal(NodeFailureMode.Restart, policy.Mode);
        Assert.Equal(9, policy.Limit);
        Assert.Equal(250, policy.BaseBackoffMs);
    }

    [Fact]
    public void FromConfig_Object_AcceptsMaxRetriesAsLimitAlias()
    {
        var config = new JsonObject { ["onError"] = new JsonObject { ["mode"] = "restart", ["maxRetries"] = 7 } };
        Assert.Equal(7, NodeFailurePolicy.FromConfig(config, NodeFailurePolicy.Fail).Limit);
    }

    [Fact]
    public void FromConfig_MissingOrUnknown_KeepsFallback()
    {
        var fallback = new NodeFailurePolicy { Mode = NodeFailureMode.Restart };
        Assert.Equal(fallback, NodeFailurePolicy.FromConfig(new JsonObject(), fallback));
        Assert.Equal(fallback.Mode, NodeFailurePolicy.FromConfig(new JsonObject { ["onError"] = "nonsense" }, fallback).Mode);
    }

    [Fact]
    public void BackoffFor_IsExponential_AndCapped()
    {
        var p = new NodeFailurePolicy { BaseBackoffMs = 100, MaxBackoffMs = 500 };
        Assert.Equal(100, p.BackoffFor(1).TotalMilliseconds);
        Assert.Equal(200, p.BackoffFor(2).TotalMilliseconds);
        Assert.Equal(400, p.BackoffFor(3).TotalMilliseconds);
        Assert.Equal(500, p.BackoffFor(4).TotalMilliseconds);   // capped
        Assert.Equal(500, p.BackoffFor(9).TotalMilliseconds);
    }

    [Fact]
    public void AllowsAttempt_UnboundedWhenLimitIsZero()
    {
        var forever = new NodeFailurePolicy { Mode = NodeFailureMode.Restart, Limit = 0 };
        Assert.True(forever.AllowsAttempt(1));
        Assert.True(forever.AllowsAttempt(1_000_000));

        var bounded = new NodeFailurePolicy { Mode = NodeFailureMode.Restart, Limit = 2 };
        Assert.True(bounded.AllowsAttempt(2));
        Assert.False(bounded.AllowsAttempt(3));

        Assert.False(NodeFailurePolicy.Fail.AllowsAttempt(1)); // fail never restarts
    }

    // ---- fakes ----

    private static IFrameEnvelope Frame(int i) => new BinaryFrameEnvelope("cam", i, $"f{i}.bmp", [(byte)i], "image/bmp");

    /// <summary>Throws on the first N runs, then yields one result (or exhausts). Plain INodeRunner (not rewindable).</summary>
    private sealed class ScriptedNode(string nodeId, int throwsBeforeSuccess, string message = "run failed", int exhaustAfter = 1)
        : INodeRunner
    {
        private int _thrown;
        public int ExecuteCalls { get; private set; }
        public int ActivateCalls { get; private set; }
        public int RestartCalls => ActivateCalls - 1;
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

    private sealed class RewindableNode(string nodeId) : INodeRunner, IRewindableSource
    {
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);
        public Task RewindAsync(CancellationToken cancellationToken) => Task.CompletedTask;
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Tracks activation/disposal and runs via a supplied delegate (which may throw). Used for hard-restart tests.</summary>
    private sealed class TrackingNode(string nodeId, Func<NodeExecutionResult> run) : INodeRunner
    {
        public int ActivateCalls { get; private set; }
        public bool Disposed { get; private set; }
        public string NodeId { get; } = nodeId;
        public Task ActivateAsync(CancellationToken cancellationToken) { ActivateCalls++; return Task.CompletedTask; }
        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(run());
        public ValueTask DisposeAsync() { Disposed = true; return ValueTask.CompletedTask; }
    }

    private sealed class ThrowingEveryTimeNode(string nodeId, string message) : INodeRunner
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

    private sealed class SinkNode(string nodeId) : INodeRunner
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
