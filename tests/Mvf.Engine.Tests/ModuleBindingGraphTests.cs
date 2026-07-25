using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;
using Mvf.Engine.Values;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Values;

namespace Mvf.Engine.Tests;

/// <summary>
/// Live-editable module config from the CLI. A module reads its config only at activation, so an edit
/// re-activates the node with the new value. These pin the round trip: the pre-pass overlays a stored edit
/// before the run, the activator surfaces the field as a tunable, and a mid-run edit re-opens the node.
/// </summary>
public sealed class ModuleBindingGraphTests
{
    private static PipelineDefinition Expand(string json) =>
        new PipelineExpander().Expand(json, new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase));

    // A source drives the cycles; `worker` has a live-editable `factor`.
    private const string GraphJson = """
    {
      "nodes": [
        { "id": "ticks", "kind": "integration-module", "category": "source", "moduleId": "test.ticker",
          "outputs": [ { "name": "tick", "channel": "control", "dataType": "control/list:json" } ] },
        { "id": "worker", "kind": "integration-module", "category": "compute", "moduleId": "test.worker",
          "inputs": [ { "name": "in", "channel": "control", "dataType": "control/list:json" } ],
          "config": { "factor": 2 },
          "bindings": { "factor": { "type": "int", "binding": "worker.factor" } } }
      ],
      "edges": [ { "from": "ticks.tick", "to": "worker.in" } ]
    }
    """;

    [Fact]
    public async Task PrePass_OverlaysAStoredEditOntoConfig()
    {
        var definition = Expand(GraphJson);
        var store = new StubBindingStore { ["worker.factor"] = 9 };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        var worker = definition.Nodes.Single(n => n.Id == "worker");
        Assert.Equal(9, worker.Config["factor"]!.GetValue<int>());
    }

    [Fact]
    public async Task PrePass_RejectsAStoredEditOfTheWrongType()
    {
        var definition = Expand(GraphJson);
        var store = new StubBindingStore { ["worker.factor"] = "not-an-int" };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        Assert.Contains(result.Errors, e => e.Contains("worker.factor") && e.Contains("expected int"));
    }

    [Fact]
    public async Task ActivatingAModuleBinding_RegistersItAsATunable()
    {
        var definition = Expand(GraphJson);
        var registry = new LiveValueRegistry();

        await new PipelineGraphExecutor(
                new BindingActivator(registry, ("ticks", _ => new TickRunner("ticks", 1, _ => { })),
                    ("worker", _ => new NoopRunner("worker"))),
                liveValues: registry)
            .ExecuteAsync(
                definition,
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        var live = registry.Find("worker.factor");
        Assert.NotNull(live);
        Assert.Equal(ControlValueType.Int, live!.Type);
        Assert.Equal("worker.factor", live.Binding);
        Assert.Equal(2, live.Current!.GetValue<int>());
    }

    [Fact]
    public async Task EditingAModuleBinding_ReactivatesTheNodeWithTheNewValue()
    {
        var definition = Expand(GraphJson);
        var registry = new LiveValueRegistry();

        // Each time `worker` activates, it reads its config `factor` — so the captured list is the history
        // of values it was opened with.
        var openedWith = new List<int>();
        INodeRunner MakeWorker(PipelineNodeDefinition node)
        {
            openedWith.Add(node.Config["factor"]!.GetValue<int>());
            return new NoopRunner("worker");
        }

        // Mid-run, turn the tunable — exactly what the dashboard's edit does.
        var ticker = new TickRunner("ticks", totalTicks: 8, onTick: cycle =>
        {
            if (cycle == 3)
            {
                Assert.True(registry.TrySet("worker.factor", JsonValue.Create(9), out var error), error);
            }
        });

        var report = await new PipelineGraphExecutor(
                new BindingActivator(registry, ("ticks", _ => ticker), ("worker", MakeWorker)),
                liveValues: registry)
            .ExecuteAsync(
                definition,
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        Assert.True(report.Succeeded);

        // Opened with 2 up front, re-opened with 9 after the edit — a module takes config only at activation.
        Assert.Equal(2, openedWith[0]);
        Assert.Contains(9, openedWith);
        Assert.Equal(9, openedWith[^1]);
    }

    /// <summary>
    /// Creates a fresh runner per activation (via a factory) and registers each node's bindings as tunables,
    /// exactly as the real activator does — so the executor's re-activation path is exercised end to end.
    /// </summary>
    private sealed class BindingActivator(
        LiveValueRegistry registry,
        params (string NodeId, Func<PipelineNodeDefinition, INodeRunner> Factory)[] factories) : IPipelineNodeActivator
    {
        private readonly Dictionary<string, Func<PipelineNodeDefinition, INodeRunner>> _factories =
            factories.ToDictionary(f => f.NodeId, f => f.Factory, StringComparer.OrdinalIgnoreCase);

        public async Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node,
            PipelineExecutionOptions options,
            CancellationToken cancellationToken)
        {
            foreach (var binding in ModuleBindings.Read(node.Bindings))
            {
                var current = node.Config.TryGetPropertyValue(binding.Field, out var value) ? value?.DeepClone() : null;
                registry.Register(
                    binding.LiveKey(node.Id), binding.Field, binding.Type, binding.Schema,
                    binding.Binding, current, ControlValueShape.Single);
            }

            var runner = _factories[node.Id](node);
            await runner.ActivateAsync(cancellationToken);
            return runner;
        }
    }

    private sealed class TickRunner(string nodeId, int totalTicks, Action<int> onTick) : INodeRunner
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

            onTick(_cycle++);
            return Task.FromResult(NodeExecutionResult.Single(
                "tick", PortValue.FromControl(ControlSignal.FromList([], NodeId))));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class NoopRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class StubBindingStore : IValueBindingStore
    {
        private readonly Dictionary<string, JsonNode?> _values = new(StringComparer.Ordinal);

        public string Location => "(test)";

        public JsonNode? this[string name] { set => _values[name] = value; }

        public Task<IReadOnlyDictionary<string, JsonNode?>> LoadAsync(CancellationToken cancellationToken) =>
            Task.FromResult<IReadOnlyDictionary<string, JsonNode?>>(
                new Dictionary<string, JsonNode?>(_values, StringComparer.Ordinal));

        public Task SaveAsync(IReadOnlyDictionary<string, JsonNode?> bindings, CancellationToken cancellationToken) =>
            Task.CompletedTask;
    }
}
