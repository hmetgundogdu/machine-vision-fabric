using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;
using Mvf.Engine.Values;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

public sealed class BindingPrePassTests
{
    private static PipelineDefinition Expand(string json) =>
        new PipelineExpander().Expand(json, new Dictionary<string, ModuleCatalogEntry>(StringComparer.OrdinalIgnoreCase));

    private static PipelineNodeDefinition Node(PipelineDefinition definition, string id) =>
        definition.Nodes.Single(n => n.Id == id);

    [Fact]
    public async Task Run_LiteralNeedsNoBindingAndAsksNobody()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "literal": 40 } } ], "edges": [] }
        """);

        var resolver = new RecordingResolver();
        var store = new InMemoryBindingStore();

        var result = await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Empty(resolver.Requests);
        Assert.False(store.Saved);
    }

    [Fact]
    public async Task Run_ReadsAStoredBindingAndParksItAsAConstant()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "brightness.threshold" } } ], "edges": [] }
        """);

        var resolver = new RecordingResolver();
        var store = new InMemoryBindingStore { ["brightness.threshold"] = JsonValue.Create(55) };

        var result = await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Empty(resolver.Requests);
        Assert.Equal(55, Node(definition, "threshold").Config["resolved"]!.GetValue<int>());
    }

    [Fact]
    public async Task Run_AsksTheResolverThenStoresTheAnswerUnderItsBinding()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "brightness.threshold", "prompt": "Threshold" } } ],
          "edges": [] }
        """);

        var resolver = new RecordingResolver(JsonValue.Create(70));
        var store = new InMemoryBindingStore();

        var result = await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Equal(1, result.PromptedCount);
        Assert.True(result.BindingsChanged);
        Assert.Equal("Threshold", Assert.Single(resolver.Requests).Prompt);
        Assert.Equal(70, Node(definition, "threshold").Config["resolved"]!.GetValue<int>());
        Assert.Equal(70, store["brightness.threshold"]!.GetValue<int>());
    }

    [Fact]
    public async Task Run_FallsBackToTheDefaultWhenNobodyCanAnswer()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "folder", "primitive": "value",
                       "config": { "type": "string", "binding": "out.folder", "default": "artifacts" } } ],
          "edges": [] }
        """);

        var result = await new BindingPrePass(new InMemoryBindingStore(), resolver: null)
            .RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.False(Node(definition, "folder").Config.ContainsKey("resolved"));
    }

    [Fact]
    public async Task Run_FailsWithTheBindingNameAndWhereToSetIt()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "cameraSerial", "primitive": "value",
                       "config": { "type": "string", "binding": "camera.serial" } } ], "edges": [] }
        """);

        var store = new InMemoryBindingStore { Location = "/pkg/.mvf/bindings.json" };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        var error = Assert.Single(result.Errors);
        Assert.Contains("camera.serial", error);
        Assert.Contains("/pkg/.mvf/bindings.json", error);
    }

    [Fact]
    public async Task Run_RejectsAStoredBindingOfTheWrongType()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "brightness.threshold" } } ], "edges": [] }
        """);

        var store = new InMemoryBindingStore { ["brightness.threshold"] = JsonValue.Create("not-a-number") };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        Assert.Contains("expected int", Assert.Single(result.Errors));
    }

    [Fact]
    public async Task Run_EnforcesTheDeclaredSchemaOnAResolvedValue()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "camera", "primitive": "value",
              "config": {
                "type": "json",
                "binding": "camera.record",
                "schema": { "type": "object", "required": ["serial"] }
              } }
          ],
          "edges": []
        }
        """);

        var store = new InMemoryBindingStore { ["camera.record"] = new JsonObject { ["address"] = "10.0.0.4" } };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        Assert.Contains("serial", Assert.Single(result.Errors));
    }

    [Fact]
    public async Task Run_DoesNotWriteBindingsWhenTheRunHasErrors()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "answered", "primitive": "value", "config": { "type": "string", "binding": "a" } },
            { "id": "unanswerable", "primitive": "value", "config": { "type": "string", "binding": "b" } }
          ],
          "edges": []
        }
        """);

        // Answers the first request, then has nothing for the second.
        var resolver = new RecordingResolver(JsonValue.Create("yes")) { AnswerOnce = true };
        var store = new InMemoryBindingStore();

        var result = await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        Assert.False(store.Saved);
    }

    [Fact]
    public async Task Run_SelectWithACriterionInConfigNeedsNothing()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "pickCam", "primitive": "select",
                       "config": { "mode": "one", "by": "serial", "where": "ABC123" } } ], "edges": [] }
        """);

        var resolver = new RecordingResolver();

        var result = await new BindingPrePass(new InMemoryBindingStore(), resolver)
            .RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Empty(resolver.Requests);
    }

    [Fact]
    public async Task Run_SelectWithAWiredCriterionPortIsLeftToRunTime()
    {
        var definition = Expand("""
        {
          "nodes": [
            { "id": "threshold", "primitive": "value", "config": { "type": "string", "literal": "ABC123" } },
            { "id": "pickCam", "primitive": "select", "config": { "mode": "one", "type": "string" } }
          ],
          "edges": [ { "from": "threshold.value", "to": "pickCam.criterion" } ]
        }
        """);

        var resolver = new RecordingResolver();

        var result = await new BindingPrePass(new InMemoryBindingStore(), resolver)
            .RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Empty(resolver.Requests);
        Assert.False(Node(definition, "pickCam").Config.ContainsKey("resolvedCriterion"));
    }

    private const string PickerJson = """
    {
      "nodes": [
        { "id": "cameras", "primitive": "value",
          "config": { "type": "json", "shape": "list",
                      "literal": [
                        { "serial": "ABC123", "protocol": "gige" },
                        { "serial": "DEF456", "protocol": "gige" }
                      ] } },
        { "id": "pickCam", "primitive": "select",
          "config": { "mode": "one", "by": "serial", "binding": "camera", "prompt": "Select a camera" } }
      ],
      "edges": [ { "from": "cameras.value", "to": "pickCam.items" } ]
    }
    """;

    [Fact]
    public async Task Run_OffersTheUpstreamCollectionAsChoices()
    {
        var definition = Expand(PickerJson);
        var resolver = new RecordingResolver(JsonValue.Create("DEF456"));

        var result = await new BindingPrePass(new InMemoryBindingStore(), resolver)
            .RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);

        var request = Assert.Single(resolver.Requests);
        Assert.NotNull(request.Choices);
        Assert.Equal(2, request.Choices.Count);
        Assert.Equal("ABC123", request.Choices[0]!["serial"]!.GetValue<string>());
        Assert.Equal("serial", request.ChoiceLabelProperty);
        Assert.Equal("Select a camera", request.Prompt);
    }

    [Fact]
    public async Task Run_ResolvesTheUpstreamCollectionBeforeOfferingIt()
    {
        // The list itself comes from a binding, and the select is declared first in the file — only
        // topological order gets the candidates settled before the picker is offered.
        var definition = Expand("""
        {
          "nodes": [
            { "id": "pickCam", "primitive": "select",
              "config": { "mode": "one", "by": "serial", "binding": "camera" } },
            { "id": "cameras", "primitive": "value",
              "config": { "type": "json", "shape": "list", "binding": "candidates" } }
          ],
          "edges": [ { "from": "cameras.value", "to": "pickCam.items" } ]
        }
        """);

        var store = new InMemoryBindingStore
        {
            ["candidates"] = new JsonArray(new JsonObject { ["serial"] = "ZZZ999" })
        };
        var resolver = new RecordingResolver(JsonValue.Create("ZZZ999"));

        await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        var request = Assert.Single(resolver.Requests);
        Assert.Equal("ZZZ999", Assert.Single(request.Choices!)!["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task Run_OffersNoChoicesWhenTheCollectionIsNotKnowable()
    {
        // Null rather than an empty list: an empty picker reads as "none found", which is a different
        // statement from "not known yet".
        var definition = Expand("""
        { "nodes": [ { "id": "pickCam", "primitive": "select",
                       "config": { "mode": "one", "by": "serial", "binding": "camera" } } ], "edges": [] }
        """);

        var resolver = new RecordingResolver(JsonValue.Create("ABC123"));
        await new BindingPrePass(new InMemoryBindingStore(), resolver).RunAsync(definition, CancellationToken.None);

        Assert.Null(Assert.Single(resolver.Requests).Choices);
    }

    [Fact]
    public async Task Run_PickerAnswerIsStoredSoLaterRunsNeverAsk()
    {
        var store = new InMemoryBindingStore();

        var first = Expand(PickerJson);
        var picker = new RecordingResolver(JsonValue.Create("DEF456"));
        await new BindingPrePass(store, picker).RunAsync(first, CancellationToken.None);

        Assert.Equal("DEF456", store["camera"]!.GetValue<string>());

        var second = Expand(PickerJson);
        var silent = new RecordingResolver(JsonValue.Create("DEF456"));
        var result = await new BindingPrePass(store, silent).RunAsync(second, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Empty(silent.Requests);
        Assert.Equal("DEF456", Node(second, "pickCam").Config["resolvedCriterion"]!.GetValue<string>());
    }

    [Fact]
    public async Task Run_SelectReadsItsCriterionFromTheBindingStore()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "pickCam", "primitive": "select",
                       "config": { "mode": "one", "by": "serial", "binding": "camera" } } ], "edges": [] }
        """);

        var store = new InMemoryBindingStore { ["camera"] = JsonValue.Create("DEF456") };

        var result = await new BindingPrePass(store, resolver: null).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.Equal("DEF456", Node(definition, "pickCam").Config["resolvedCriterion"]!.GetValue<string>());
    }

    [Fact]
    public async Task Run_DoesNotCacheATransientAnswer()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "brightness.threshold" } } ], "edges": [] }
        """);

        // An environment-style resolver: durable at its source, so caching it would make this run's value
        // outrank every later change to the variable.
        var resolver = new RecordingResolver(JsonValue.Create(55)) { Transient = true };
        var store = new InMemoryBindingStore();

        var result = await new BindingPrePass(store, resolver).RunAsync(definition, CancellationToken.None);

        Assert.True(result.Succeeded);
        Assert.False(result.BindingsChanged);
        Assert.False(store.Saved);
        Assert.Equal(55, Node(definition, "threshold").Config["resolved"]!.GetValue<int>());
    }

    [Fact]
    public async Task Run_TurnsAThrowingResolverIntoAReadableError()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "t" } } ], "edges": [] }
        """);

        var result = await new BindingPrePass(new InMemoryBindingStore(), new ThrowingResolver())
            .RunAsync(definition, CancellationToken.None);

        Assert.False(result.Succeeded);
        var error = Assert.Single(result.Errors);
        Assert.Contains("threshold", error);
        Assert.Contains("the terminal exploded", error);
    }

    [Fact]
    public async Task Run_IsIdempotent_ASecondPassAsksNobodyAgain()
    {
        var definition = Expand("""
        { "nodes": [ { "id": "threshold", "primitive": "value",
                       "config": { "type": "int", "binding": "t" } } ], "edges": [] }
        """);

        var resolver = new RecordingResolver(JsonValue.Create(9));
        var store = new InMemoryBindingStore();
        var prePass = new BindingPrePass(store, resolver);

        await prePass.RunAsync(definition, CancellationToken.None);
        await prePass.RunAsync(definition, CancellationToken.None);

        Assert.Single(resolver.Requests);
    }

    private sealed class InMemoryBindingStore : IValueBindingStore
    {
        private readonly Dictionary<string, JsonNode?> _values = new(StringComparer.Ordinal);

        public string Location { get; set; } = "(memory)";

        public bool Saved { get; private set; }

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
            Saved = true;
            _values.Clear();
            foreach (var (name, value) in bindings)
            {
                _values[name] = value;
            }

            return Task.CompletedTask;
        }
    }

    /// <summary>Stands in for a resolver whose host code fails — a broken terminal, a studio disconnect.</summary>
    private sealed class ThrowingResolver : IValueResolver
    {
        public bool CanResolve => true;

        public Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken) =>
            throw new InvalidOperationException("the terminal exploded");
    }

    private sealed class RecordingResolver(JsonNode? answer = null) : IValueResolver
    {
        public List<ValueRequest> Requests { get; } = [];

        /// <summary>Answers the first request and nothing after it, to exercise a partly-resolved run.</summary>
        public bool AnswerOnce { get; init; }

        /// <summary>Answers like an environment variable: usable, but not to be cached.</summary>
        public bool Transient { get; init; }

        public bool CanResolve => true;

        public Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken)
        {
            var first = Requests.Count == 0;
            Requests.Add(request);

            if (answer is null || (AnswerOnce && !first))
            {
                return Task.FromResult(ValueResolution.Unresolved);
            }

            return Task.FromResult(Transient
                ? ValueResolution.Transient(answer.DeepClone())
                : ValueResolution.Ok(answer.DeepClone()));
        }
    }
}
