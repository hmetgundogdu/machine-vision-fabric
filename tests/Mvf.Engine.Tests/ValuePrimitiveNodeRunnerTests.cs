using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Graph.Values;

namespace Mvf.Engine.Tests;

public sealed class ValuePrimitiveNodeRunnerTests
{
    private static (ValuePrimitiveNodeRunner Runner, LiveValueRegistry Registry) Build(
        string nodeId,
        JsonNode? initial,
        ControlValueType type = ControlValueType.Json,
        JsonNode? schema = null)
    {
        var registry = new LiveValueRegistry();
        var live = registry.Register(nodeId, nodeId, type, schema, binding: nodeId, initial);
        return (new ValuePrimitiveNodeRunner(nodeId, live), registry);
    }

    private static JsonNode? Emitted(NodeExecutionResult result) => result.Get("value")!.Control!.Payload;

    [Fact]
    public async Task Execute_EmitsTheResolvedValueOnTheValuePort()
    {
        var (runner, _) = Build("threshold", JsonValue.Create(42), ControlValueType.Int);

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        var value = result.Get("value");
        Assert.NotNull(value);
        Assert.True(value.IsControl);
        Assert.Equal("value", value.Control!.SignalType);
        Assert.Equal(42, value.Control.Payload!.GetValue<int>());
    }

    [Fact]
    public async Task Execute_EmitsTheSameConstantEveryCycle()
    {
        var (runner, _) = Build("outputFolder", JsonValue.Create("D:/captures"), ControlValueType.String);

        var first = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);
        var second = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        Assert.Equal("D:/captures", Emitted(first)!.GetValue<string>());
        Assert.Equal("D:/captures", Emitted(second)!.GetValue<string>());
    }

    [Fact]
    public async Task Execute_CarriesAJsonObjectWhole()
    {
        var camera = new JsonObject { ["serial"] = "ABC123", ["address"] = "192.168.0.7" };
        var (runner, _) = Build("cameraRecord", camera);

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        var payload = Assert.IsType<JsonObject>(Emitted(result));
        Assert.Equal("ABC123", payload["serial"]!.GetValue<string>());
    }

    // ── live tuning ───────────────────────────────────────────────────────────────────────────────

    [Fact]
    public async Task Execute_PicksUpALiveChangeOnTheNextCycle()
    {
        var (runner, registry) = Build("threshold", JsonValue.Create(40), ControlValueType.Int);

        var before = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);
        Assert.True(registry.TrySet("threshold", JsonValue.Create(90), out _));
        var after = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        Assert.Equal(40, Emitted(before)!.GetValue<int>());
        Assert.Equal(90, Emitted(after)!.GetValue<int>());
    }

    [Fact]
    public async Task Execute_DoesNotSeeAChangeMadeAfterItRan()
    {
        // The cycle that already emitted keeps its value: a setting takes effect on the *next* pass, which
        // is what makes tuning safe to do while frames are in flight.
        var (runner, registry) = Build("threshold", JsonValue.Create(40), ControlValueType.Int);

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);
        registry.TrySet("threshold", JsonValue.Create(90), out _);

        Assert.Equal(40, Emitted(result)!.GetValue<int>());
    }

    [Fact]
    public void TrySet_RejectsAValueOfTheWrongType()
    {
        var (_, registry) = Build("threshold", JsonValue.Create(40), ControlValueType.Int);

        var ok = registry.TrySet("threshold", JsonValue.Create("ninety"), out var error);

        Assert.False(ok);
        Assert.Equal("expected int", error);
        Assert.Equal(40, registry.Find("threshold")!.Current!.GetValue<int>());
    }

    [Fact]
    public void TrySet_RejectsAValueThatBreaksTheDeclaredSchema()
    {
        var schema = new JsonObject { ["type"] = "integer", ["minimum"] = 0, ["maximum"] = 255 };
        var (_, registry) = Build("threshold", JsonValue.Create(40), ControlValueType.Int, schema);

        Assert.False(registry.TrySet("threshold", JsonValue.Create(900), out var error));
        Assert.Contains("255", error);
        Assert.True(registry.TrySet("threshold", JsonValue.Create(200), out _));
    }

    [Fact]
    public void TrySet_AppliesAListSchemaToEachElementNotToTheArray()
    {
        // The bug this pins: a schema describing an element, checked against the whole array, rejects
        // every list ("expected object but found array") — including a perfectly good one.
        var registry = new LiveValueRegistry();
        var schema = new JsonObject { ["type"] = "object", ["required"] = new JsonArray("serial") };
        registry.Register("cameras", "cameras", ControlValueType.Json, schema, binding: null,
            initial: new JsonArray(new JsonObject { ["serial"] = "A" }), shape: ControlValueShape.List);

        Assert.True(registry.TrySet(
            "cameras",
            new JsonArray(new JsonObject { ["serial"] = "B" }, new JsonObject { ["serial"] = "C" }),
            out var error), error);

        Assert.False(registry.TrySet(
            "cameras",
            new JsonArray(new JsonObject { ["serial"] = "B" }, new JsonObject { ["protocol"] = "gige" }),
            out var rejected));
        Assert.Contains("[1]", rejected);
        Assert.Contains("serial", rejected);
    }

    [Fact]
    public void TrySet_RaisesChangedSoAHostCanPersistIt()
    {
        var (_, registry) = Build("threshold", JsonValue.Create(40), ControlValueType.Int);
        var seen = new List<(string NodeId, string? Binding, int Value)>();
        registry.Changed += live => seen.Add((live.NodeId, live.Binding, live.Current!.GetValue<int>()));

        registry.TrySet("threshold", JsonValue.Create(77), out _);
        registry.TrySet("threshold", JsonValue.Create("bad"), out _);

        var change = Assert.Single(seen);
        Assert.Equal(("threshold", "threshold", 77), change);
    }

    [Fact]
    public void TrySet_OnAnUnknownNodeFails()
    {
        var registry = new LiveValueRegistry();

        Assert.False(registry.TrySet("nope", JsonValue.Create(1), out var error));
        Assert.Contains("not a tunable value", error);
    }
}
