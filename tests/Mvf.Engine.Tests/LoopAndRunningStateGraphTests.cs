using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;
using Mvf.Graph.Execution;
using Mvf.Graph.Integrations;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Values;

namespace Mvf.Engine.Tests;

/// <summary>
/// End to end over the <c>loop</c> as the graph's iteration authority. Every node runs every cycle (no
/// setup, no region); the loop owns the termination policy (<c>until-exhausted</c> / <c>forever</c> /
/// <c>count</c>) and carries a whole-graph pause that stops it advancing without tearing anything down.
/// </summary>
public sealed class LoopAndRunningStateGraphTests
{
    // A source drives the cycles; the loop's mode decides when to stop. The loop's `done` back-edge is
    // optional, so these functional tests leave it unwired and vary only the mode.
    private static string GraphWith(string loopConfig) => $$"""
    {
      "name": "loop-graph",
      "nodes": [
        { "id": "ticks", "kind": "integration-module", "category": "source", "moduleId": "test.ticker",
          "outputs": [ { "name": "tick", "channel": "control", "dataType": "control/list:json" } ] },
        { "id": "cycle", "primitive": "loop", "config": {{loopConfig}} },
        { "id": "sink", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
          "inputs": [ { "name": "in", "channel": "control", "dataType": "control/list:json" } ] }
      ],
      "edges": [ { "from": "ticks.tick", "to": "sink.in" } ]
    }
    """;

    private static PipelineDefinition Expand(string json) =>
        new PipelineExpander().Expand(json, new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase));

    private static PipelineExecutionOptions Options(int maxCycles = 0) =>
        new() { PackageRoot = ".", IntegrationsRoot = ".", MaxCycles = maxCycles };

    [Fact]
    public void LoopGraph_IsValid()
    {
        var result = new PipelineDefinitionValidator().Validate(Expand(GraphWith("""{ "mode": "until-exhausted" }""")));
        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public async Task EveryNodeRunsEveryCycle_NoNodeRunsOnce()
    {
        var definition = Expand(GraphWith("""{ "mode": "until-exhausted" }"""));
        var sink = new CapturingConsumerRunner("sink");
        var ticker = new CountedTickRunner("ticks", totalTicks: 5);

        var report = await new PipelineGraphExecutor(
                new StubbingActivator(null, ("ticks", ticker), ("sink", sink)))
            .ExecuteAsync(definition, Options(), CancellationToken.None);

        Assert.True(report.Succeeded);
        // No setup, no region: the consumer ran on every one of the 5 cycles.
        Assert.Equal(5, sink.Received.Count);
    }

    [Fact]
    public async Task ForeverMode_RewindsTheSourceAndKeepsGoing()
    {
        var definition = Expand(GraphWith("""{ "mode": "forever" }"""));
        var sink = new CapturingConsumerRunner("sink");
        var ticker = new RewindableTickRunner("ticks", framesPerPass: 3);

        // The source has only 3 frames per pass; forever rewinds it, so the run reaches the 7-cycle cap.
        var report = await new PipelineGraphExecutor(
                new StubbingActivator(null, ("ticks", ticker), ("sink", sink)))
            .ExecuteAsync(definition, Options(maxCycles: 7), CancellationToken.None);

        Assert.Equal(7, report.TotalCycles);
        Assert.Equal(7, sink.Received.Count);
        Assert.Equal(2, ticker.RewindCount);   // 3 + rewind + 3 + rewind + 1
    }

    [Fact]
    public async Task CountMode_StopsAfterExactlyNCycles()
    {
        var definition = Expand(GraphWith("""{ "mode": "count", "count": 4 }"""));
        var sink = new CapturingConsumerRunner("sink");
        var ticker = new CountedTickRunner("ticks", totalTicks: 100);

        var report = await new PipelineGraphExecutor(
                new StubbingActivator(null, ("ticks", ticker), ("sink", sink)))
            .ExecuteAsync(definition, Options(), CancellationToken.None);

        Assert.True(report.Succeeded);
        Assert.Equal(4, report.TotalCycles);
        Assert.Equal(4, sink.Received.Count);
    }

    [Fact]
    public async Task Pausing_FreezesTheLoop_AndResumingContinuesWhereItLeftOff()
    {
        var definition = Expand(GraphWith("""{ "mode": "until-exhausted" }"""));
        var registry = new LiveValueRegistry();
        var sink = new CapturingConsumerRunner("sink");
        var ticker = new PacedTickRunner("ticks", perTickMs: 10);

        var cycles = 0;
        var firstCycle = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var options = Options() with
        {
            OnCycleCompleted = p => { Volatile.Write(ref cycles, p.TotalCycles); firstCycle.TrySetResult(); }
        };

        using var cts = new CancellationTokenSource();
        var run = new PipelineGraphExecutor(
                new StubbingActivator(registry, ("ticks", ticker), ("sink", sink)),
                liveValues: registry)
            .ExecuteAsync(definition, options, cts.Token);

        // Let the run get going, then pause and let any in-flight cycle drain.
        await firstCycle.Task.WaitAsync(TimeSpan.FromSeconds(5));
        registry.RunControl.Pause();
        await Task.Delay(120);
        var atPause = Volatile.Read(ref cycles);

        // While paused the loop does not advance at all — no cycle completes.
        await Task.Delay(250);
        Assert.Equal(atPause, Volatile.Read(ref cycles));

        // Pause is not cancel: the run is alive and its state is intact, so resuming just carries on.
        registry.RunControl.Resume();
        Assert.True(await WaitUntilAsync(() => Volatile.Read(ref cycles) > atPause, TimeSpan.FromSeconds(5)));

        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => run);
    }

    private static async Task<bool> WaitUntilAsync(Func<bool> condition, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (condition())
            {
                return true;
            }

            await Task.Delay(20);
        }

        return condition();
    }

    /// <summary>
    /// Stubs the module nodes and hands every primitive (the <c>loop</c>) to the real activator, so the
    /// mode logic and pause are exercised against the actual runner and executor.
    /// </summary>
    private sealed class StubbingActivator(
        LiveValueRegistry? liveValues,
        params (string NodeId, INodeRunner Runner)[] stubs) : IPipelineNodeActivator
    {
        private readonly Dictionary<string, INodeRunner> _stubs =
            stubs.ToDictionary(s => s.NodeId, s => s.Runner, StringComparer.OrdinalIgnoreCase);

        private readonly PipelineNodeActivator _real =
            new(new NoModuleLoader(), new EmptySimulatorSourceCatalog(), new ModuleCatalog(),
                outOfProcessModuleHost: null, liveValues: liveValues);

        public async Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node,
            PipelineExecutionOptions options,
            CancellationToken cancellationToken)
        {
            if (_stubs.TryGetValue(node.Id, out var stub))
            {
                await stub.ActivateAsync(cancellationToken);
                return stub;
            }

            return await _real.ActivateAsync(node, options, cancellationToken);
        }
    }

    private sealed class NoModuleLoader : IIntegrationModuleLoader
    {
        public IReadOnlyList<IIntegrationModule> LoadModules(string integrationsRoot) => [];
    }

    /// <summary>Drives a fixed number of cycles, then reports exhaustion like any source.</summary>
    private sealed class CountedTickRunner(string nodeId, int totalTicks) : INodeRunner
    {
        private int _cycle;

        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_cycle >= totalTicks)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _cycle++;
            return Task.FromResult(NodeExecutionResult.Single(
                "tick", PortValue.FromControl(ControlSignal.FromList([], NodeId))));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>A source of <paramref name="framesPerPass"/> frames that the loop can rewind for `forever`.</summary>
    private sealed class RewindableTickRunner(string nodeId, int framesPerPass) : INodeRunner, IRewindableSource
    {
        private int _emitted;

        public string NodeId { get; } = nodeId;

        public int RewindCount { get; private set; }

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_emitted >= framesPerPass)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _emitted++;
            return Task.FromResult(NodeExecutionResult.Single(
                "tick", PortValue.FromControl(ControlSignal.FromList([], NodeId))));
        }

        public Task RewindAsync(CancellationToken cancellationToken)
        {
            _emitted = 0;
            RewindCount++;
            return Task.CompletedTask;
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>Drives cycles indefinitely, pacing each one so pause/resume can be observed between them.</summary>
    private sealed class PacedTickRunner(string nodeId, int perTickMs) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public async Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            await Task.Delay(perTickMs, cancellationToken);
            return NodeExecutionResult.Single("tick", PortValue.FromControl(ControlSignal.FromList([], NodeId)));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class CapturingConsumerRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public List<JsonNode?> Received { get; } = [];

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("in")?.Control is { } signal)
            {
                Received.Add(signal.Payload);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
