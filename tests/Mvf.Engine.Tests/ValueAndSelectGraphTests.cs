using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;
using Mvf.Engine.Values;
using Mvf.Graph.Execution;
using Mvf.Graph.Integrations;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Values;

namespace Mvf.Engine.Tests;

/// <summary>
/// End to end over the whole contract: a binding pre-pass resolves the criterion before cycle 0, the
/// real activator builds the primitives from their config, and the serial executor routes typed control
/// values along real edges — <c>discover → select → consumer</c>, which is the camera picker with the
/// hardware left out.
/// </summary>
public sealed class ValueAndSelectGraphTests
{
    private static JsonArray Cameras() =>
    [
        new JsonObject { ["serial"] = "ABC123", ["address"] = "10.0.0.4" },
        new JsonObject { ["serial"] = "DEF456", ["address"] = "10.0.0.5" }
    ];

    private const string PickerJson = """
    {
      "name": "camera-picker",
      "nodes": [
        { "id": "discover", "kind": "integration-module", "category": "source", "moduleId": "test.discovery",
          "outputs": [ { "name": "cameras", "channel": "control", "dataType": "control/list:json" } ] },
        { "id": "pickCam", "primitive": "select",
          "config": { "mode": "one", "by": "serial", "binding": "camera" } },
        { "id": "useCam", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
          "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:json" } ] }
      ],
      "edges": [
        { "from": "discover.cameras", "to": "pickCam.items" },
        { "from": "pickCam.selected", "to": "useCam.device" }
      ]
    }
    """;

    private static PipelineDefinition Expand(string json) =>
        new PipelineExpander().Expand(json, new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase));

    [Fact]
    public void PickerGraph_IsValid()
    {
        var result = new PipelineDefinitionValidator().Validate(Expand(PickerJson));

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public async Task StoredBinding_NarrowsDiscoveryToOneCameraWithoutAskingAnyone()
    {
        var definition = Expand(PickerJson);
        var store = new StubBindingStore { ["camera"] = "DEF456" };

        var prePass = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);
        Assert.True(prePass.Succeeded);

        var consumer = new CapturingConsumerRunner("useCam");
        var report = await Run(definition, consumer);

        Assert.True(report.Succeeded);
        var device = Assert.IsType<JsonObject>(Assert.Single(consumer.Received));
        Assert.Equal("DEF456", device["serial"]!.GetValue<string>());
        Assert.Equal("10.0.0.5", device["address"]!.GetValue<string>());
    }

    [Fact]
    public async Task AnsweredOnce_TheBindingIsWrittenAndTheNextRunIsSilent()
    {
        var store = new StubBindingStore();

        // First run: nothing stored, so the resolver is asked and its answer persisted.
        var first = Expand(PickerJson);
        var resolver = new StubResolver("ABC123");
        var firstPass = await new BindingPrePass(store, resolver).RunAsync(first, CancellationToken.None);

        Assert.True(firstPass.Succeeded);
        Assert.Equal(1, firstPass.PromptedCount);
        Assert.Equal("ABC123", store["camera"]!.GetValue<string>());

        // Second run on a fresh copy of the same pipeline file: the binding answers, nobody is asked.
        var second = Expand(PickerJson);
        var silentResolver = new StubResolver("ABC123");
        var secondPass = await new BindingPrePass(store, silentResolver).RunAsync(second, CancellationToken.None);

        Assert.True(secondPass.Succeeded);
        Assert.Equal(0, secondPass.PromptedCount);
        Assert.Empty(silentResolver.Requests);

        var consumer = new CapturingConsumerRunner("useCam");
        await Run(second, consumer);

        Assert.Equal("ABC123", Assert.IsType<JsonObject>(Assert.Single(consumer.Received))["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task TwoMachinesBindTheSamePipelineFileToDifferentCameras()
    {
        var panelA = new StubBindingStore { ["camera"] = "ABC123" };
        var panelB = new StubBindingStore { ["camera"] = "DEF456" };

        var onA = Expand(PickerJson);
        var onB = Expand(PickerJson);

        await new BindingPrePass(panelA, resolver: null).RunAsync(onA, CancellationToken.None);
        await new BindingPrePass(panelB, resolver: null).RunAsync(onB, CancellationToken.None);

        var consumerA = new CapturingConsumerRunner("useCam");
        var consumerB = new CapturingConsumerRunner("useCam");
        await Run(onA, consumerA);
        await Run(onB, consumerB);

        Assert.Equal("ABC123", Assert.IsType<JsonObject>(consumerA.Received[0])["serial"]!.GetValue<string>());
        Assert.Equal("DEF456", Assert.IsType<JsonObject>(consumerB.Received[0])["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task ValueNodeFeedsTheCriterionOnARealControlEdge()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "discover", "kind": "integration-module", "category": "source", "moduleId": "test.discovery",
              "outputs": [ { "name": "cameras", "channel": "control", "dataType": "control/list:json" } ] },
            { "id": "wanted", "primitive": "value", "config": { "type": "json", "literal": "ABC123" } },
            { "id": "pickCam", "primitive": "select", "config": { "mode": "one", "by": "serial" } },
            { "id": "useCam", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
              "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:json" } ] }
          ],
          "edges": [
            { "from": "discover.cameras", "to": "pickCam.items" },
            { "from": "wanted.value", "to": "pickCam.criterion" },
            { "from": "pickCam.selected", "to": "useCam.device" }
          ]
        }
        """);

        Assert.True(new PipelineDefinitionValidator().Validate(definition).IsValid);

        var prePass = await new BindingPrePass(new StubBindingStore(), resolver: null)
            .RunAsync(definition, CancellationToken.None);
        Assert.True(prePass.Succeeded);

        var consumer = new CapturingConsumerRunner("useCam");
        await Run(definition, consumer);

        Assert.Equal("ABC123", Assert.IsType<JsonObject>(Assert.Single(consumer.Received))["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task UnresolvedValueFailsActivationRatherThanRunningOnNothing()
    {
        // The pre-pass is skipped deliberately: an unresolved value must stop the run with a name to fix.
        var definition = Expand("""
        {
          "nodes": [
            { "id": "discover", "kind": "integration-module", "category": "source", "moduleId": "test.discovery",
              "outputs": [ { "name": "cameras", "channel": "control", "dataType": "control/list:json" } ] },
            { "id": "wanted", "primitive": "value", "config": { "type": "string", "binding": "camera.serial" } },
            { "id": "pickCam", "primitive": "select", "config": { "mode": "one", "type": "string", "by": "serial" } }
          ],
          "edges": [
            { "from": "discover.cameras", "to": "pickCam.items" },
            { "from": "wanted.value", "to": "pickCam.criterion" }
          ]
        }
        """);

        var report = await Run(definition, new CapturingConsumerRunner("unused"));

        Assert.False(report.Succeeded);
        Assert.Contains("camera.serial", report.ErrorMessage);
    }

    [Fact]
    public async Task ATunedValueReachesTheGraphOnTheNextCycleWithoutStoppingTheRun()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "ticks", "kind": "integration-module", "category": "source", "moduleId": "test.ticker",
              "outputs": [ { "name": "tick", "channel": "control", "dataType": "control/list:json" } ] },
            { "id": "threshold", "primitive": "value",
              "config": { "type": "int", "binding": "brightness.threshold", "literal": 40 } },
            { "id": "watch", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
              "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:int" } ] }
          ],
          "edges": [ { "from": "threshold.value", "to": "watch.device" } ]
        }
        """);

        var registry = new LiveValueRegistry();
        var consumer = new CapturingConsumerRunner("watch");

        // Turns the value once, mid-run, from outside the executor — exactly what the dashboard's edit
        // prompt does. The run is never paused for it.
        var ticker = new TickRunner("ticks", totalTicks: 6, onTick: cycle =>
        {
            if (cycle == 3)
            {
                Assert.True(registry.TrySet("threshold", JsonValue.Create(200), out _));
            }
        });

        var activator = new HybridActivator(registry, ("ticks", ticker), ("watch", consumer));
        var report = await new PipelineGraphExecutor(activator).ExecuteAsync(
            definition,
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
            CancellationToken.None);

        Assert.True(report.Succeeded);

        var seen = consumer.Received.Select(v => v!.GetValue<int>()).ToList();
        Assert.Equal(40, seen[0]);
        Assert.Contains(200, seen);
        Assert.Equal(200, seen[^1]);

        // The change is a step, not a flicker: 40s then 200s, never back.
        var firstTuned = seen.IndexOf(200);
        Assert.All(seen.Take(firstTuned), v => Assert.Equal(40, v));
        Assert.All(seen.Skip(firstTuned), v => Assert.Equal(200, v));
    }

    [Fact]
    public async Task ATunedValueIsTypeCheckedBeforeItReachesTheGraph()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "ticks", "kind": "integration-module", "category": "source", "moduleId": "test.ticker",
              "outputs": [ { "name": "tick", "channel": "control", "dataType": "control/list:json" } ] },
            { "id": "threshold", "primitive": "value",
              "config": { "type": "int", "literal": 40,
                          "schema": { "type": "integer", "minimum": 0, "maximum": 255 } } },
            { "id": "watch", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
              "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:int" } ] }
          ],
          "edges": [ { "from": "threshold.value", "to": "watch.device" } ]
        }
        """);

        var registry = new LiveValueRegistry();
        var consumer = new CapturingConsumerRunner("watch");
        var rejections = new List<string?>();

        var ticker = new TickRunner("ticks", totalTicks: 4, onTick: cycle =>
        {
            if (cycle != 2)
            {
                return;
            }

            registry.TrySet("threshold", JsonValue.Create("ninety"), out var typeError);
            rejections.Add(typeError);
            registry.TrySet("threshold", JsonValue.Create(900), out var boundError);
            rejections.Add(boundError);
        });

        await new PipelineGraphExecutor(new HybridActivator(registry, ("ticks", ticker), ("watch", consumer)))
            .ExecuteAsync(
                definition,
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        Assert.Equal("expected int", rejections[0]);
        Assert.Contains("255", rejections[1]);

        // A running graph is the last place that should receive an ill-typed value: nothing changed.
        Assert.All(consumer.Received, v => Assert.Equal(40, v!.GetValue<int>()));
    }

    /// <summary>
    /// Three value nodes wired to two selects — the shape a pipeline takes when it needs several values.
    /// Each is its own node, which is the point: `value` produces one value and is not a form.
    /// </summary>
    private const string WiredValuesJson = """
    {
      "nodes": [
        { "id": "cameras", "primitive": "value",
          "config": { "type": "json", "shape": "list",
                      "literal": [
                        { "serial": "ABC123", "protocol": "gige" },
                        { "serial": "DEF456", "protocol": "gige" },
                        { "serial": "GHI789", "protocol": "usb" }
                      ] } },
        { "id": "wantedCamera",   "primitive": "value", "config": { "type": "json", "literal": "DEF456" } },
        { "id": "wantedProtocol", "primitive": "value", "config": { "type": "json", "literal": "gige" } },

        { "id": "pickOne",      "primitive": "select", "config": { "mode": "one",  "by": "serial" } },
        { "id": "pickProtocol", "primitive": "select", "config": { "mode": "many", "by": "protocol" } },

        { "id": "watchOne", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
          "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:json" } ] },
        { "id": "watchMany", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
          "inputs": [ { "name": "device", "channel": "control", "dataType": "control/list:json" } ] }
      ],
      "edges": [
        { "from": "cameras.value",        "to": "pickOne.items" },
        { "from": "wantedCamera.value",   "to": "pickOne.criterion" },
        { "from": "cameras.value",        "to": "pickProtocol.items" },
        { "from": "wantedProtocol.value", "to": "pickProtocol.criterion" },
        { "from": "pickOne.selected",     "to": "watchOne.device" },
        { "from": "pickProtocol.selected","to": "watchMany.device" }
      ]
    }
    """;

    [Fact]
    public void ThreeValuesWiredToTwoSelects_IsValid()
    {
        var result = new PipelineDefinitionValidator().Validate(Expand(WiredValuesJson));

        Assert.True(result.IsValid, string.Join("; ", result.Issues.Select(i => $"{i.Code}: {i.Message}")));
    }

    [Fact]
    public async Task ThreeValuesWiredToTwoSelects_EachSelectNarrowsWithItsOwnCriterion()
    {
        var definition = Expand(WiredValuesJson);
        var one = new CapturingConsumerRunner("watchOne");
        var many = new CapturingConsumerRunner("watchMany");
        var ticker = new TickRunner("cameras-tick", totalTicks: 2, onTick: _ => { });

        // The list value is itself the source here, so one extra node drives the cycles.
        var report = await new PipelineGraphExecutor(
            new HybridActivator(null, (ticker.NodeId, ticker), ("watchOne", one), ("watchMany", many)))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        Assert.True(report.Succeeded);

        var picked = Assert.IsType<JsonObject>(one.Received[0]);
        Assert.Equal("DEF456", picked["serial"]!.GetValue<string>());

        var kept = Assert.IsType<JsonArray>(many.Received[0]);
        Assert.Equal(2, kept.Count);
        Assert.All(kept, c => Assert.Equal("gige", c!["protocol"]!.GetValue<string>()));
    }

    [Fact]
    public async Task TuningOneValueReNarrowsOnlyItsOwnSelect()
    {
        var definition = Expand(WiredValuesJson);
        var registry = new LiveValueRegistry();
        var one = new CapturingConsumerRunner("watchOne");
        var many = new CapturingConsumerRunner("watchMany");

        var ticker = new TickRunner("cameras-tick", totalTicks: 6, onTick: cycle =>
        {
            if (cycle == 3)
            {
                Assert.True(registry.TrySet("wantedCamera", JsonValue.Create("GHI789"), out _));
            }
        });

        await new PipelineGraphExecutor(
            new HybridActivator(registry, (ticker.NodeId, ticker), ("watchOne", one), ("watchMany", many)))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        var serials = one.Received.Select(v => v!["serial"]!.GetValue<string>()).ToList();
        Assert.Equal("DEF456", serials[0]);
        Assert.Equal("GHI789", serials[^1]);

        // The other select never saw the change: three values, three independent nodes.
        Assert.All(many.Received, list => Assert.Equal(2, ((JsonArray)list!).Count));
    }

    [Fact]
    public async Task TuningTheCandidateListItselfChangesBothSelects()
    {
        var definition = Expand(WiredValuesJson);
        var registry = new LiveValueRegistry();
        var one = new CapturingConsumerRunner("watchOne");
        var many = new CapturingConsumerRunner("watchMany");

        var ticker = new TickRunner("cameras-tick", totalTicks: 6, onTick: cycle =>
        {
            if (cycle != 3)
            {
                return;
            }

            // A list-shaped value is tuned as a whole: this is the discovery module's stand-in, so
            // "re-discovering" is just setting a new collection.
            JsonArray rediscovered =
            [
                new JsonObject { ["serial"] = "GHI789", ["protocol"] = "gige" }
            ];

            Assert.True(registry.TrySet("cameras", rediscovered, out _));
        });

        await new PipelineGraphExecutor(
            new HybridActivator(registry, (ticker.NodeId, ticker), ("watchOne", one), ("watchMany", many)))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        // pickOne wanted DEF456, which the new collection no longer holds — so it stops emitting rather
        // than inventing a match, and its consumer simply does not run those cycles.
        Assert.All(one.Received, v => Assert.Equal("DEF456", v!["serial"]!.GetValue<string>()));
        Assert.True(one.Received.Count < many.Received.Count);

        // pickProtocol still matches, on a collection that is now one element long.
        Assert.Equal(2, ((JsonArray)many.Received[0]!).Count);
        Assert.Equal(1, ((JsonArray)many.Received[^1]!).Count);
    }

    private const string RunningPickerJson = """
    {
      "nodes": [
        { "id": "cameras", "primitive": "value",
          "config": { "type": "json", "shape": "list",
                      "literal": [
                        { "serial": "ABC123", "protocol": "gige" },
                        { "serial": "DEF456", "protocol": "gige" },
                        { "serial": "GHI789", "protocol": "usb" }
                      ] } },
        { "id": "pickOne", "primitive": "select",
          "config": { "mode": "one", "by": "serial", "binding": "camera", "resolvedCriterion": "ABC123" } },
        { "id": "watch", "kind": "integration-module", "category": "compute", "moduleId": "test.consumer",
          "inputs": [ { "name": "device", "channel": "control", "dataType": "control/value:json" } ] }
      ],
      "edges": [
        { "from": "cameras.value",    "to": "pickOne.items" },
        { "from": "pickOne.selected", "to": "watch.device" }
      ]
    }
    """;

    [Fact]
    public async Task ARunningSelectPublishesTheCandidatesItIsNarrowing()
    {
        var definition = Expand(RunningPickerJson);
        var registry = new LiveValueRegistry();
        var watch = new CapturingConsumerRunner("watch");
        var offered = new List<int>();

        var ticker = new TickRunner("tick", totalTicks: 3, onTick: _ =>
            offered.Add(registry.Find("pickOne")?.Choices?.Count ?? 0));

        await new PipelineGraphExecutor(
            new HybridActivator(registry, ("tick", ticker), ("watch", watch)))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        // Nothing to offer before the node has seen its items; three candidates from the first cycle on.
        Assert.Equal(0, offered[0]);
        Assert.Equal(3, offered[^1]);

        var live = registry.Find("pickOne")!;
        Assert.Equal("serial", live.ChoiceLabelProperty);
        Assert.Equal("ABC123", live.Choices![0]!["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task ChoosingAnotherCandidateMidRunReNarrowsOnTheNextCycle()
    {
        var definition = Expand(RunningPickerJson);
        var registry = new LiveValueRegistry();
        var watch = new CapturingConsumerRunner("watch");

        var ticker = new TickRunner("tick", totalTicks: 6, onTick: cycle =>
        {
            if (cycle != 3)
            {
                return;
            }

            // What the dashboard's picker does: take a candidate the node is currently narrowing and store
            // the property named by `by`, not the whole record.
            var live = registry.Find("pickOne")!;
            var chosen = live.Choices!.Single(c => c!["serial"]!.GetValue<string>() == "GHI789");
            Assert.True(registry.TrySet("pickOne", chosen!["serial"]!.DeepClone(), out var error), error);
        });

        await new PipelineGraphExecutor(
            new HybridActivator(registry, ("tick", ticker), ("watch", watch)))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        var serials = watch.Received.Select(v => v!["serial"]!.GetValue<string>()).ToList();
        Assert.Equal("ABC123", serials[0]);
        Assert.Equal("GHI789", serials[^1]);

        // A step, not a flicker — and the whole record still travels, only the criterion was a property.
        var firstChanged = serials.IndexOf("GHI789");
        Assert.All(serials.Take(firstChanged), s => Assert.Equal("ABC123", s));
        Assert.All(serials.Skip(firstChanged), s => Assert.Equal("GHI789", s));
        Assert.Equal("usb", watch.Received[^1]!["protocol"]!.GetValue<string>());
    }

    [Fact]
    public async Task ASelectFedByAnEdgeIsNotOfferedAsATunable()
    {
        // Tuning it would be overwritten by the edge on the very next cycle, and a control that silently
        // does nothing is worse than no control. The pre-pass marks it; the activator skips registering it.
        var definition = Expand(WiredValuesJson);
        var registry = new LiveValueRegistry();

        await new BindingPrePass(new StubBindingStore(), resolver: null)
            .RunAsync(definition, CancellationToken.None);

        var ticker = new TickRunner("cameras-tick", totalTicks: 1, onTick: _ => { });
        await new PipelineGraphExecutor(
            new HybridActivator(registry, (ticker.NodeId, ticker),
                ("watchOne", new CapturingConsumerRunner("watchOne")),
                ("watchMany", new CapturingConsumerRunner("watchMany"))))
            .ExecuteAsync(
                WithTicker(definition, ticker),
                new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
                CancellationToken.None);

        // pickProtocol takes its criterion from an edge; pickOne in that graph does too.
        Assert.Null(registry.Find("pickProtocol"));
        Assert.Null(registry.Find("pickOne"));

        // The value nodes behind those edges are the tunables instead.
        Assert.NotNull(registry.Find("wantedProtocol"));
        Assert.NotNull(registry.Find("cameras"));
    }

    [Fact]
    public void TuningAListRejectsAHeterogeneousCollection()
    {
        var registry = new LiveValueRegistry();
        registry.Register("cameras", "cameras", ControlValueType.Int, schema: null, binding: null,
            initial: new JsonArray(1, 2), shape: ControlValueShape.List);

        Assert.False(registry.TrySet("cameras", new JsonArray(1, "two"), out var error));
        Assert.Equal("expected a list of int", error);
    }

    /// <summary>Adds a source node whose only job is to drive cycles, plus its edge into nothing.</summary>
    private static PipelineDefinition WithTicker(PipelineDefinition definition, TickRunner ticker)
    {
        var nodes = definition.Nodes.ToList();
        nodes.Insert(0, new PipelineNodeDefinition
        {
            Id = ticker.NodeId,
            Kind = "integration-module",
            Category = "source",
            ModuleId = "test.ticker",
            Outputs = [new PipelinePortDefinition { Name = "tick", Channel = "control", DataType = "control/list:json" }]
        });

        return new PipelineDefinition
        {
            Name = definition.Name,
            Nodes = nodes,
            Edges = definition.Edges
        };
    }

    private static Task<PipelineExecutionReport> Run(PipelineDefinition definition, CapturingConsumerRunner consumer)
    {
        var activator = new HybridActivator(
            liveValues: null,
            ("discover", new DiscoveryRunner("discover", Cameras())),
            (consumer.NodeId, consumer));

        return new PipelineGraphExecutor(activator).ExecuteAsync(
            definition,
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = "." },
            CancellationToken.None);
    }

    /// <summary>
    /// Stubs the module nodes and hands every primitive to the real <see cref="PipelineNodeActivator"/>,
    /// so what is under test includes the activator reading value/select config.
    /// </summary>
    private sealed class HybridActivator(
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

    /// <summary>Publishes the discovered collection once, then reports exhaustion like any other source.</summary>
    private sealed class DiscoveryRunner(string nodeId, JsonArray cameras) : INodeRunner
    {
        private bool _published;

        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_published)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _published = true;
            return Task.FromResult(NodeExecutionResult.Single(
                "cameras", PortValue.FromControl(ControlSignal.FromList(cameras, NodeId))));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    /// <summary>
    /// Drives a fixed number of cycles and runs <paramref name="onTick"/> at the top of each one — the
    /// hook that stands in for an operator editing a value while frames are in flight.
    /// </summary>
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

    private sealed class CapturingConsumerRunner(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public List<JsonNode?> Received { get; } = [];

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (inputs.Get("device")?.Control is { } signal)
            {
                Received.Add(signal.Payload);
            }

            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class StubBindingStore : IValueBindingStore
    {
        private readonly Dictionary<string, JsonNode?> _values = new(StringComparer.Ordinal);

        public string Location => "(test)";

        public JsonNode? this[string name]
        {
            get => _values.GetValueOrDefault(name);
            set => _values[name] = value;
        }

        public Task<IReadOnlyDictionary<string, JsonNode?>> LoadAsync(CancellationToken cancellationToken) =>
            Task.FromResult<IReadOnlyDictionary<string, JsonNode?>>(
                new Dictionary<string, JsonNode?>(_values, StringComparer.Ordinal));

        public Task SaveAsync(IReadOnlyDictionary<string, JsonNode?> bindings, CancellationToken cancellationToken)
        {
            _values.Clear();
            foreach (var (name, value) in bindings)
            {
                _values[name] = value;
            }

            return Task.CompletedTask;
        }
    }

    private sealed class StubResolver(string answer) : IValueResolver
    {
        public List<ValueRequest> Requests { get; } = [];

        public bool CanResolve => true;

        public Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken)
        {
            Requests.Add(request);
            return Task.FromResult(ValueResolution.Ok(JsonValue.Create(answer)));
        }
    }
}
